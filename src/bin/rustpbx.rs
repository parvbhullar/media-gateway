use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use rustpbx::{
    app::{AppStateBuilder, create_router},
    config::Config,
    handler::api_v1::auth::issue_api_key,
    handler::middleware::request_log::AccessLogEventFormat,
    models::{api_key, create_db},
    observability, preflight, version,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tokio::time::{Duration, sleep};
use tracing::info;
use tracing_subscriber::{
    EnvFilter, fmt::time::LocalTime, layer::SubscriberExt, util::SubscriberInitExt,
};

#[derive(Parser, Debug)]
#[command(
    author,
    version = version::get_short_version(),
    about = "A versatile SIP PBX server implemented in Rust",
    long_about = version::get_version_info()
)]
struct Cli {
    /// Path to the configuration file
    #[clap(
        long,
        global = true,
        help = "Path to the configuration file (TOML format)"
    )]
    conf: Option<String>,
    #[clap(
        long,
        global = true,
        help = "Tokio console server address, e.g. /tmp/tokio-console or 127.0.0.1:5556"
    )]
    tokio_console: Option<String>,
    #[cfg(feature = "console")]
    #[clap(
        long,
        global = true,
        requires = "super_password",
        help = "Create or update a console super user before starting the server"
    )]
    super_username: Option<String>,
    #[cfg(feature = "console")]
    #[clap(
        long,
        global = true,
        requires = "super_username",
        help = "Password for the console super user"
    )]
    super_password: Option<String>,
    #[cfg(feature = "console")]
    #[clap(
        long,
        global = true,
        requires = "super_username",
        help = "Email for the console super user (defaults to username@localhost)"
    )]
    super_email: Option<String>,
    #[clap(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Validate configuration and exit without starting the server
    CheckConfig,
    /// Initialize with fixture data (extensions, routes, wholesale demo data)
    Fixtures,
    /// Manage `/api/v1/*` Bearer-token API keys.
    #[command(subcommand)]
    ApiKey(ApiKeyCmd),
}

#[derive(Subcommand, Debug)]
enum ApiKeyCmd {
    /// Create a new API key. Prints the plaintext token exactly once.
    Create {
        /// Human-readable name. Must be unique.
        name: String,
        /// Optional free-form description stored alongside the hash.
        #[arg(long)]
        description: Option<String>,
    },
    /// List every API key row (active + revoked).
    List,
    /// Revoke an API key by name. Subsequent requests will fail with 401.
    Revoke {
        /// Name of the key to revoke.
        name: String,
    },
}

async fn run_api_key_cmd(database_url: &str, cmd: ApiKeyCmd) -> Result<()> {
    let db = create_db(database_url).await?;
    match cmd {
        ApiKeyCmd::Create { name, description } => {
            let issued = issue_api_key();
            let am = api_key::ActiveModel {
                name: Set(name.clone()),
                hash_sha256: Set(issued.hash.clone()),
                description: Set(description),
                created_at: Set(Utc::now()),
                ..Default::default()
            };
            am.insert(&db).await?;
            println!("API key created. Store this token — it will NOT be shown again:");
            println!("  {}", issued.plaintext);
            println!("name={} sha256={}", name, issued.hash);
        }
        ApiKeyCmd::List => {
            let rows = api_key::Entity::find().all(&db).await?;
            if rows.is_empty() {
                println!("(no api keys)");
            } else {
                for r in rows {
                    let status = if r.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    };
                    println!(
                        "{:<24} {:<8} created={} last_used={:?}",
                        r.name, status, r.created_at, r.last_used_at
                    );
                }
            }
        }
        ApiKeyCmd::Revoke { name } => {
            let row = api_key::Entity::find()
                .filter(api_key::Column::Name.eq(name.clone()))
                .one(&db)
                .await?
                .ok_or_else(|| anyhow::anyhow!("api key '{}' not found", name))?;
            let mut am: api_key::ActiveModel = row.into();
            am.revoked_at = Set(Some(Utc::now()));
            am.update(&db).await?;
            println!("Revoked {}", name);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    sqlx::any::install_default_drivers();

    dotenv().ok();
    let cli = Cli::parse();

    let config_path = cli.conf.clone();

    // Merge base config with DB overrides → config.generated.toml.
    // Falls back to the original path if the merge step fails (e.g. no DB yet).
    let effective_config_path = if let Some(ref path) = config_path {
        match rustpbx::config_merge::apply(path).await {
            Ok(generated) => {
                println!("Config merged: {}", generated);
                Some(generated)
            }
            Err(e) => {
                println!("Config merge skipped ({}), using base config.", e);
                Some(path.clone())
            }
        }
    } else {
        None
    };

    let config = if let Some(ref path) = effective_config_path {
        println!("Loading config from: {}", path);
        Config::load(path).expect("Failed to load config")
    } else {
        println!("Loading default config");
        Config::default()
    };

    println!("Start at {}", Utc::now());
    println!("{}", version::get_version_info());

    if matches!(cli.command, Some(Commands::Fixtures)) {
        let state = AppStateBuilder::new()
            .with_config(config.clone())
            .with_config_metadata(config_path.clone(), Utc::now())
            .with_skip_sip_bind()
            .build()
            .await
            .expect("Failed to build app state for fixtures");

        state
            .addon_registry
            .initialize_all(state.clone())
            .await
            .expect("Failed to initialize addons for fixtures");

        rustpbx::fixtures::run_fixtures(state).await?;
        println!("Fixtures initialized successfully.");
        return Ok(());
    }

    if matches!(cli.command, Some(Commands::ApiKey(_))) {
        let Some(Commands::ApiKey(cmd)) = cli.command else {
            unreachable!()
        };
        run_api_key_cmd(&config.database_url, cmd).await?;
        return Ok(());
    }

    if matches!(cli.command, Some(Commands::CheckConfig)) {
        match preflight::validate_start(&config).await {
            Ok(_) => {
                println!("Configuration is valid; all required sockets are available.");
                return Ok(());
            }
            Err(err) => {
                eprintln!("Configuration validation failed:");
                for issue in err.issues {
                    eprintln!("- {}: {}", issue.field, issue.message);
                }
                std::process::exit(1);
            }
        }
    }

    #[cfg(feature = "console")]
    if let Some(super_username) = cli.super_username.as_deref() {
        let super_password = cli
            .super_password
            .as_deref()
            .expect("super_password is required when super_username is provided");
        let super_email = cli
            .super_email
            .as_deref()
            .map(|email| email.to_string())
            .unwrap_or_else(|| format!("{}@localhost", super_username));

        let db = rustpbx::models::create_db(&config.database_url)
            .await
            .expect("Failed to create or connect to database");

        rustpbx::models::user::Model::upsert_super_user(
            &db,
            super_username,
            &super_email,
            super_password,
        )
        .await
        .expect("Failed to create or update super user");
        println!(
            "Console super user '{}' ensured with email '{}'",
            super_username, super_email
        );
        return Ok(());
    }

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let mut env_filter = if let Some(level) = config.log_level.as_deref() {
            EnvFilter::new(level)
        } else {
            EnvFilter::new("info")
        };

        // Suppress noisy third-party crates to warn level by default when
        // the user did not provide an explicit RUST_LOG override.
        for noisy in &["hyper_util", "rustls", "sqlx"] {
            if let Ok(d) = format!("{}=warn", noisy).parse() {
                env_filter = env_filter.add_directive(d);
            }
        }

        env_filter
    });

    // Install the hot-swappable reload layer BEFORE the subscriber is built.
    // The commercial TelemetryAddon will inject an OTel layer into this slot
    // during addon initialization.
    let otel_reload_layer = observability::init_reload_layer();

    let tokio_console_enabled = cli.tokio_console.is_some()
        || std::env::var_os("TOKIO_CONSOLE").is_some()
        || std::env::var_os("TOKIO_CONSOLE_BIND").is_some();
    let mut console_layer = None;
    if tokio_console_enabled {
        use console_subscriber::ServerAddr;
        let mut builder = console_subscriber::ConsoleLayer::builder()
            .retention(std::time::Duration::from_secs(60));
        if let Some(addr) = &cli.tokio_console {
            builder = match addr.parse::<SocketAddr>() {
                Ok(sock) => builder.server_addr(ServerAddr::Tcp(sock)),
                Err(_) => {
                    #[cfg(unix)]
                    {
                        builder.server_addr(ServerAddr::Unix(PathBuf::from(addr)))
                    }
                    #[cfg(not(unix))]
                    {
                        tracing::warn!(
                            "tokio-console unix socket path '{}' is not supported on this target, falling back to 127.0.0.1:6669",
                            addr
                        );
                        builder.server_addr(ServerAddr::Tcp(
                            "127.0.0.1:6669".parse().expect("valid socket addr"),
                        ))
                    }
                }
            };
        } else {
            builder = {
                #[cfg(unix)]
                {
                    builder.server_addr(ServerAddr::Unix(PathBuf::from("/tmp/tokio-console")))
                }
                #[cfg(not(unix))]
                {
                    builder.server_addr(ServerAddr::Tcp(
                        "127.0.0.1:6669".parse().expect("valid socket addr"),
                    ))
                }
            };
        }
        console_layer = Some(builder.spawn());
    }
    let mut file_layer = None;
    let mut guard_holder = None;
    let mut fmt_layer = None;
    if let Some(ref log_file) = config.log_file {
        let log_path = std::path::Path::new(log_file);
        let dir = log_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let prefix = log_path
            .file_name()
            .expect("log_file must have a file name")
            .to_string_lossy();
        let appender = match config.log_rotation.to_lowercase().as_str() {
            "hourly" => tracing_appender::rolling::hourly(dir, prefix.as_ref()),
            "daily" => tracing_appender::rolling::daily(dir, prefix.as_ref()),
            _ => tracing_appender::rolling::never(dir, prefix.as_ref()),
        };
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        guard_holder = Some(guard);
        file_layer = Some(
            tracing_subscriber::fmt::layer()
                .with_timer(LocalTime::rfc_3339())
                .event_format(AccessLogEventFormat::new(LocalTime::rfc_3339()))
                .with_ansi(false)
                .with_writer(non_blocking),
        );
    } else {
        fmt_layer = Some(
            tracing_subscriber::fmt::layer()
                .with_timer(LocalTime::rfc_3339())
                .event_format(AccessLogEventFormat::new(LocalTime::rfc_3339())),
        );
    }
    // Every branch receives the same OTel reload layer so that the commercial
    // TelemetryAddon can inject a live OTel tracing layer later, regardless of
    // which logging backend was chosen.
    if let Some(console_layer) = console_layer {
        tracing_subscriber::registry()
            .with(otel_reload_layer)
            .with(env_filter)
            .with(console_layer)
            .try_init()?;
    } else if let Some(file_layer) = file_layer {
        tracing_subscriber::registry()
            .with(otel_reload_layer)
            .with(env_filter)
            .with(file_layer)
            .try_init()?;
    } else if let Some(fmt_layer) = fmt_layer {
        tracing_subscriber::registry()
            .with(otel_reload_layer)
            .with(env_filter)
            .with(fmt_layer)
            .try_init()?;
    }

    let _ = guard_holder; // keep the guard alive

    let mut cached_config = Some(config);
    // base_config_path is always config.toml — used as the merge source on every restart.
    // effective_config_path (config.generated.toml) is stored in app_state so the
    // console settings page reads the actual merged/effective values from disk.
    // base_config_path is always config.toml — used as the merge source on every restart.
    // effective_config_path (config.generated.toml) is stored in app_state so the
    // console settings page reads the actual merged/effective values from disk.
    let base_config_path = config_path.clone();
    let mut current_effective_path = effective_config_path.clone();
    let mut retry_count = 0;
    let max_retries = 10;
    let retry_interval = Duration::from_secs(5);

    loop {
        let config = if let Some(cfg) = cached_config.take() {
            cfg
        } else if let Some(ref path) = base_config_path {
            // Always re-merge from base config.toml (not the generated file) so DB
            // overrides are applied fresh and never stacked on top of themselves.
            let effective = match rustpbx::config_merge::apply(path).await {
                Ok(generated) => {
                    current_effective_path = Some(generated.clone());
                    generated
                }
                Err(e) => {
                    tracing::warn!("Config merge skipped on restart ({}), using base.", e);
                    current_effective_path = Some(path.clone());
                    path.clone()
                }
            };
            match Config::load(&effective) {
                Ok(cfg) => cfg,
                Err(err) => {
                    retry_count += 1;
                    if retry_count > max_retries {
                        return Err(anyhow::anyhow!(
                            "Failed to load config from {} after {} retries: {}",
                            path,
                            max_retries,
                            err
                        ));
                    }
                    tracing::error!(
                        "Failed to load config from {} (retry {}/{}): {}. Retrying in {:?}...",
                        path,
                        retry_count,
                        max_retries,
                        err,
                        retry_interval
                    );
                    sleep(retry_interval).await;
                    continue;
                }
            }
        } else {
            Config::default()
        };

        let state_builder = AppStateBuilder::new()
            .with_config(config.clone())
            // Store effective (generated) path so the console settings page reads merged values.
            .with_config_metadata(current_effective_path.clone(), Utc::now());

        let (app_reload_requested, app_config_path) = {
            let state = match state_builder.build().await {
                Ok(state) => state,
                Err(err) => {
                    retry_count += 1;
                    if retry_count > max_retries {
                        return Err(anyhow::anyhow!(
                            "Failed to build app after {} retries: {}",
                            max_retries,
                            err
                        ));
                    }
                    tracing::error!(
                        "Failed to build app (retry {}/{}): {}. Retrying in {:?}...",
                        retry_count,
                        max_retries,
                        err,
                        retry_interval
                    );
                    sleep(retry_interval).await;
                    cached_config = Some(config);
                    continue;
                }
            };

            info!("starting rustpbx on {}", state.config().http_addr);
            let router = create_router(state.clone());
            let mut app_future = Box::pin(rustpbx::app::run(state.clone(), router));

            #[cfg(unix)]
            let mut sigterm_stream = {
                use tokio::signal::unix::{SignalKind, signal};
                signal(SignalKind::terminate()).expect("failed to install signal handler")
            };

            let mut reload_requested = false;
            let mut app_exit_err = None;

            #[cfg(unix)]
            {
                tokio::select! {
                    result = &mut app_future => {
                        if let Err(err) = result {
                            app_exit_err = Some(err);
                        } else {
                            reload_requested = state.reload_requested.load(Ordering::Relaxed);
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        info!("received CTRL+C, shutting down");
                        state.token().cancel();
                        let _ = app_future.await;
                    }
                    _ = sigterm_stream.recv() => {
                        info!("received SIGTERM, shutting down");
                        state.token().cancel();
                        let _ = app_future.await;
                    }
                }
            }

            #[cfg(not(unix))]
            {
                tokio::select! {
                    result = &mut app_future => {
                        if let Err(err) = result {
                            app_exit_err = Some(err);
                        } else {
                            reload_requested = state.reload_requested.load(Ordering::Relaxed);
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        info!("received CTRL+C, shutting down");
                        state.token().cancel();
                        let _ = app_future.await;
                    }
                }
            }

            if let Some(err) = app_exit_err {
                retry_count += 1;
                if retry_count > max_retries {
                    return Err(anyhow::anyhow!(
                        "Application failed after {} retries: {}",
                        max_retries,
                        err
                    ));
                }
                tracing::error!(
                    "Application error (retry {}/{}): {}. Retrying in {:?}...",
                    retry_count,
                    max_retries,
                    err,
                    retry_interval
                );
                sleep(retry_interval).await;
                cached_config = Some(config);
                continue;
            }
            (reload_requested, state.config_path.clone())
        };

        if app_reload_requested {
            info!("Reload requested; restarting with updated configuration");
            let _ = app_config_path; // noted but base_config_path is always used for merge
            cached_config = None;
            retry_count = 0;

            sleep(Duration::from_secs(3)).await; // give some time for sockets to be released
            continue;
        }

        break;
    }

    // Flush any buffered OTel spans before the process exits.
    #[cfg(feature = "addon-telemetry")]
    rustpbx::addons::telemetry::TelemetryAddon::shutdown();

    Ok(())
}
