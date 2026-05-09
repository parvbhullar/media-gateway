use crate::app::AppStateInner;
use crate::handler::api_v1::auth::issue_api_key;
use crate::models::api_key::{
    ActiveModel as ApiKeyActiveModel, Column as ApiKeyColumn, Entity as ApiKeyEntity,
};
use crate::models::system_config;
use crate::config::{
    CallRecordConfig, Config, HttpRouterConfig, LocatorWebhookConfig, ProxyConfig,
    UserBackendConfig,
};
use crate::console::handlers::forms;
use crate::console::{ConsoleState, middleware::AuthRequired};
use crate::models::department::{
    ActiveModel as DepartmentActiveModel, Column as DepartmentColumn, Entity as DepartmentEntity,
};
use crate::models::rbac::{
    ActiveModel as RoleActiveModel, Column as RoleColumn, Entity as RoleEntity, role_permission,
    user_role,
};
use crate::models::user::{
    ActiveModel as UserActiveModel, Column as UserColumn, Entity as UserEntity, Model as UserModel,
};
use crate::rwi::auth::RwiConfig;
use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHasher, SaltString};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{KeepAlive, Sse};
use futures::stream;
use std::convert::Infallible;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use sea_orm::sea_query::Condition;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::time::Duration as StdDuration;
use std::{fs, sync::Arc};
use tokio::time;
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};
use tracing::warn;

#[derive(Debug, Clone, Deserialize, Default)]
struct QueryDepartmentFilters {
    pub q: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct QueryUserFilters {
    pub q: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LogRecentQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LogFollowQuery {
    pub position: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LogStreamQuery {
    pub position: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
struct DepartmentPayload {
    pub name: String,
    pub display_label: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub manager_contact: Option<String>,
    #[serde(default)]
    pub metadata: Option<JsonValue>,
}

#[derive(Debug, Clone, Deserialize)]
struct UserPayload {
    pub email: String,
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub is_staff: Option<bool>,
    #[serde(default)]
    pub is_superuser: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct RolePayload {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub permissions: Vec<PermissionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct PermissionEntry {
    pub resource: String,
    pub action: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AssignRolesPayload {
    pub role_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ProxySettingsPayload {
    pub realms: Option<Vec<String>>,
    pub locator_webhook: Option<LocatorWebhookConfig>,
    pub user_backends: Option<Vec<UserBackendConfig>>,
    pub http_router: Option<HttpRouterConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TestLocatorWebhookPayload {
    pub url: String,
    pub headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TestHttpRouterPayload {
    pub url: String,
    pub headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TestUserBackendPayload {
    pub backend: UserBackendConfig,
}

#[derive(Debug, Clone, Serialize)]
struct UserView {
    pub id: i64,
    pub email: String,
    pub username: String,
    pub last_login_at: Option<DateTime<Utc>>,
    pub last_login_ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_active: bool,
    pub is_staff: bool,
    pub is_superuser: bool,
}

impl From<UserModel> for UserView {
    fn from(model: UserModel) -> Self {
        Self {
            id: model.id,
            email: model.email,
            username: model.username,
            last_login_at: model.last_login_at,
            last_login_ip: model.last_login_ip,
            created_at: model.created_at,
            updated_at: model.updated_at,
            is_active: model.is_active,
            is_staff: model.is_staff,
            is_superuser: model.is_superuser,
        }
    }
}

pub fn urls() -> Router<Arc<ConsoleState>> {
    let router = Router::new()
        .route("/settings", get(page_settings))
        .route("/settings/config", get(get_effective_config))
        .route("/settings/config/entry", patch(upsert_config_entry))
        .route("/settings/config/entry/{key}", delete(delete_config_entry))
        .route("/settings/logs/recent", get(fetch_recent_logs))
        .route("/settings/logs/follow", get(follow_logs))
        .route("/settings/logs/stream", get(stream_logs))
        .route("/settings/config/platform", patch(update_platform_settings))
        .route("/settings/config/proxy", patch(update_proxy_settings))
        .route("/settings/config/storage", patch(update_storage_settings))
        .route(
            "/settings/config/storage/test",
            post(test_storage_connection),
        )
        .route(
            "/settings/config/proxy/locator-webhook/test",
            post(test_locator_webhook),
        )
        .route(
            "/settings/config/proxy/http-router/test",
            post(test_http_router),
        )
        .route(
            "/settings/config/proxy/user-backend/test",
            post(test_user_backend),
        )
        .route("/settings/config/security", patch(update_security_settings))
        .route("/settings/config/display", patch(update_display_settings))
        .route("/settings/config/rwi", patch(update_rwi_settings))
        // Failed S3 upload retry queue (read + ops)
        .route(
            "/settings/uploads/pending",
            get(list_pending_uploads),
        )
        .route(
            "/settings/uploads/pending/retry",
            post(retry_pending_uploads),
        )
        .route(
            "/settings/uploads/pending/clear",
            post(clear_pending_failures),
        );

    #[cfg(feature = "commerce")]
    let router = router
        .route("/settings/config/cluster", patch(update_cluster_settings))
        .route("/settings/config/cluster/reload", get(cluster_reload_sse_handler))
        .route("/settings/config/cluster/reload-addons", get(list_reload_addons_handler));

    router
        .route(
            "/settings/departments",
            post(query_departments).put(create_department),
        )
        .route(
            "/settings/departments/{id}",
            get(get_department)
                .patch(update_department)
                .delete(delete_department),
        )
        .route("/settings/users", post(query_users).put(create_user))
        .route(
            "/settings/users/{id}",
            get(get_user).patch(update_user).delete(delete_user),
        )
        .route(
            "/settings/users/{id}/roles",
            get(get_user_roles).post(assign_user_roles),
        )
        .route("/settings/roles", get(list_roles).post(create_role))
        .route(
            "/settings/roles/{id}",
            get(get_role).patch(update_role).delete(delete_role_handler),
        )
        .route("/settings/api-keys", get(list_api_keys).post(create_api_key))
        .route(
            "/settings/api-keys/{id}/revoke",
            post(revoke_api_key),
        )
}

pub async fn page_settings(
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let settings = build_settings_payload(&state).await;
    let current_user = state.build_current_user_ctx(&user).await;

    state.render_with_headers(
        "console/settings.html",
        json!({
            "nav_active": "settings",
            "settings": settings,
            "settings_data": settings,
            "username": user.username,
            "email": user.email,
            "current_user": current_user,
            "user_is_superuser": user.is_superuser,
        }),
        &headers,
    )
}

async fn build_settings_payload(state: &ConsoleState) -> JsonValue {
    let mut data = serde_json::Map::new();
    let now = Utc::now();
    let mut ami_endpoint = "/ami/v1".to_string();

    let mut platform = json!({});
    let mut proxy = json!({});
    let mut config_meta = json!({ "key_items": [] });
    let mut acl = json!({
        "active_rules": [],
        "embedded_count": 0usize,
        "file_patterns": [],
        "reload_supported": false,
        "metrics": JsonValue::Null,
    });
    let mut operations: Vec<JsonValue> = Vec::new();
    let mut console_meta = JsonValue::Null;

    let mut proxy_stats_value = JsonValue::Null;

    if let Some(app_state) = state.app_state() {
        let config_arc = app_state.config().clone();
        ami_endpoint = config_arc.proxy.ami_path.clone().unwrap_or_else(|| "/ami/v1".to_string());
        let mut loaded_config: Option<Config> = None;

        if let Some(path) = app_state.config_path.as_ref() {
            match Config::load(path) {
                Ok(cfg) => {
                    loaded_config = Some(cfg);
                }
                Err(err) => {
                    warn!(config_path = %path, ?err, "failed to reload config from disk");
                }
            }
        }

        let config = loaded_config.as_ref().unwrap_or(config_arc.as_ref());

        let uptime_duration = now - app_state.uptime;
        let uptime_seconds = uptime_duration.num_seconds().max(0);
        platform = json!({
            "version": crate::version::get_short_version(),
            "uptime_seconds": uptime_seconds,
            "uptime_pretty": human_duration(uptime_duration),
            "http_addr": config.http_addr.clone(),
            "log_level": config.log_level.clone(),
            "log_file": config.log_file.clone(),
            "config_loaded_at": app_state.config_loaded_at.to_rfc3339(),
            "config_path": app_state.config_path.clone(),
            "generated_at": now.to_rfc3339(),
        });

        let recorder_path = config.recorder_path();

        let mut key_items: Vec<JsonValue> = Vec::new();
        key_items.push(json!({ "label": "HTTP address", "value": config.http_addr.clone() }));
        if let Some(ext) = config.external_ip.as_ref() {
            key_items.push(json!({ "label": "External IP", "value": ext }));
        }
        if let (Some(start), Some(end)) = (config.rtp_start_port, config.rtp_end_port) {
            key_items.push(json!({ "label": "RTP ports", "value": format!("{}-{}", start, end) }));
        }
        key_items.push(json!({ "label": "Recorder path", "value": recorder_path.clone() }));

        if let Some(ref console_cfg) = config.console {
            key_items.push(
                json!({ "label": "Console base path", "value": console_cfg.base_path.clone() }),
            );
        }
        if let Some(ref ami_cfg) = config.ami {
            let allows = ami_cfg
                .allows
                .as_ref()
                .map(|items| items.join(", "))
                .unwrap_or_else(|| "127.0.0.1, ::1".to_string());
            key_items.push(json!({ "label": "AMI allow list", "value": allows }));
        }
        key_items.push(
            json!({ "label": "Config loaded", "value": app_state.config_loaded_at.to_rfc3339() }),
        );
        if let Some(ref path) = app_state.config_path {
            key_items.push(json!({ "label": "Config path", "value": path.clone() }));
        }
        if let Some(summary) = summarize_callrecord(config.callrecord.as_ref()) {
            key_items.push(summary);
        }
        config_meta = json!({ "key_items": key_items });

        let stats = app_state.sip_server().inner.endpoint.inner.get_stats();
        proxy_stats_value = json!({
            "transactions": {
                "running": stats.running_transactions,
                "finished": stats.finished_transactions,
                "waiting_ack": stats.waiting_ack,
            },
            "dialogs": app_state.sip_server().inner.dialog_layer.len(),
        });

        proxy = json!({
            "enabled": true,
            "addr": config.proxy.addr.clone(),
            "ports": build_port_list(&config.proxy),
            "modules": config.proxy.modules.clone().unwrap_or_default(),
            "max_concurrency": config.proxy.max_concurrency,
            "registrar_expires": config.proxy.registrar_expires,
            "callid_suffix": config.proxy.callid_suffix.clone(),
            "useragent": config.proxy.useragent.clone(),
            "ua_whitelist": config.proxy.ua_white_list.clone().unwrap_or_default(),
            "ua_blacklist": config.proxy.ua_black_list.clone().unwrap_or_default(),
            "data_sources": json!({
                "routes": "toml",
                "trunks": "toml",
            }),
            "rtp": config.rtp_config(),
            "user_backends": config.proxy.user_backends.clone(),
            "locator_webhook": config.proxy.locator_webhook.clone(),
            "http_router": config.proxy.http_router.clone(),
            "realms": config.proxy.realms.clone().unwrap_or_default(),
            "stats": proxy_stats_value.clone(),
        });

        let (active_rules, embedded_count) = resolve_acl_rules(app_state.clone()).await;
        let acl_files = &config.proxy.acl_files;
        acl = json!({
            "active_rules": active_rules,
            "embedded_count": embedded_count,
            "file_patterns": acl_files,
            "reload_supported": true,
            "metrics": JsonValue::Null,
        });

        operations.push(json!({
            "id": "reload-acl",
            "label": "Reload ACL rules",
            "description": "Re-read ACL definitions from config files and embedded lists.",
            "method": "POST",
            "endpoint": format!("{}/reload/acl", ami_endpoint.trim_end_matches('/')),
        }));

        if app_state.config_path.is_some() {
            operations.push(json!({
                "id": "reload-app",
                "label": "Reload application",
                "description": "Validate the configuration file and restart core services.",
                "method": "POST",
                "endpoint": format!("{}/reload/app", ami_endpoint.trim_end_matches('/')),
            }));
        }

        let (storage_meta, storage_profiles) = build_storage_profiles(config);

        data.insert("storage".to_string(), storage_meta.clone());
        data.insert(
            "storage_profiles".to_string(),
            JsonValue::Array(storage_profiles.clone()),
        );

        data.insert(
            "server".to_string(),
            json!({
                "operations": operations.clone(),
                "storage": storage_meta,
                "storage_profiles": storage_profiles,
            }),
        );

        console_meta = config
            .console
            .as_ref()
            .map(|cfg| {
                json!({
                    "base_path": cfg.base_path,
                    "allow_registration": cfg.allow_registration,
                })
            })
            .unwrap_or(JsonValue::Null);

        let recording_meta = config
            .recording
            .as_ref()
            .and_then(|policy| serde_json::to_value(policy).ok())
            .unwrap_or(JsonValue::Null);
        data.insert("recording".to_string(), recording_meta);
    } else {
        data.insert("storage".to_string(), json!({ "mode": "unknown" }));
        data.insert(
            "storage_profiles".to_string(),
            JsonValue::Array(Vec::<JsonValue>::new()),
        );
        data.insert(
            "server".to_string(),
            json!({
                "operations": operations.clone(),
                "storage": {"mode": "unknown"},
                "storage_profiles": Vec::<JsonValue>::new(),
            }),
        );
        data.insert("recording".to_string(), JsonValue::Null);
    }

    let stats = json!({
        "generated_at": now.to_rfc3339(),
        "proxy": proxy_stats_value,
    });

    data.insert("platform".to_string(), platform);
    data.insert("proxy".to_string(), proxy);
    data.insert("config".to_string(), config_meta);
    data.insert("acl".to_string(), acl);
    data.insert("stats".to_string(), stats);
    data.insert("ami_endpoint".to_string(), json!(ami_endpoint));
    data.insert(
        "operations".to_string(),
        JsonValue::Array(operations.clone()),
    );
    data.insert("console".to_string(), console_meta);

    // Add RWI configuration
    let rwi_config = if let Some(app_state) = state.app_state() {
        let config_arc = app_state.config().clone();
        config_arc.rwi.clone().unwrap_or_default()
    } else {
        RwiConfig::default()
    };
    data.insert(
        "rwi".to_string(),
        serde_json::to_value(rwi_config).unwrap_or(JsonValue::Null),
    );

    {
        let cluster: Option<crate::config::ClusterConfig> =
            if let Some(app_state) = state.app_state() {
                let cluster_config = app_state
                    .cluster_config
                    .read()
                    .map(|c| c.clone())
                    .unwrap_or(None);
                cluster_config.or_else(|| {
                    // fallback: read from backing config (stale snapshot)
                    // This should not normally happen after startup.
                    let config_arc = app_state.config().clone();
                    config_arc.cluster.clone()
                })
            } else {
                None
            };
        data.insert(
            "cluster".to_string(),
            serde_json::to_value(cluster.or_else(|| Some(crate::config::ClusterConfig::default())))
                .unwrap_or(JsonValue::Null),
        );
    }

    data.insert(
        "display".to_string(),
        json!({ "timezone": state.display_timezone() }),
    );

    JsonValue::Object(data)
}

async fn resolve_acl_rules(app_state: Arc<AppStateInner>) -> (Vec<String>, usize) {
    let context = app_state.sip_server().inner.data_context.clone();
    let snapshot = context.acl_rules_snapshot();
    let embedded = if let Some(path) = app_state.config_path.as_ref() {
        match Config::load(path) {
            Ok(cfg) => cfg
                .proxy
                .acl_rules
                .as_ref()
                .map(|rules| rules.len())
                .unwrap_or(0),
            Err(err) => {
                warn!(config_path = %path, ?err, "failed to reload config for acl snapshot");
                app_state
                    .sip_server()
                    .inner
                    .proxy_config
                    .acl_rules
                    .as_ref()
                    .map(|rules| rules.len())
                    .unwrap_or(0)
            }
        }
    } else {
        app_state
            .sip_server()
            .inner
            .proxy_config
            .acl_rules
            .as_ref()
            .map(|rules| rules.len())
            .unwrap_or(0)
    };

    (snapshot, embedded)
}

fn build_storage_profiles(config: &crate::config::Config) -> (JsonValue, Vec<JsonValue>) {
    use serde_json::Map;

    struct Profile {
        id: String,
        label: &'static str,
        description: String,
        config: Map<String, JsonValue>,
    }

    impl Profile {
        fn new(id: impl Into<String>, label: &'static str, description: impl Into<String>) -> Self {
            Self {
                id: id.into(),
                label,
                description: description.into(),
                config: Map::new(),
            }
        }

        fn insert(&mut self, key: &str, value: JsonValue) {
            self.config.insert(key.to_string(), value);
        }

        fn into_json(self) -> JsonValue {
            let mut object = Map::new();
            object.insert("id".to_string(), json!(self.id));
            object.insert("label".to_string(), json!(self.label));
            object.insert("description".to_string(), json!(self.description));
            object.insert("config".to_string(), JsonValue::Object(self.config));
            JsonValue::Object(object)
        }
    }

    let recorder_path = config.recorder_path();

    let (mode, callrecord_profile) = match config.callrecord.as_ref() {
        Some(CallRecordConfig::Local { root }) => {
            let mut profile = Profile::new(
                "callrecord-local",
                "Call recordings",
                format!("Storing call detail records on {}", root),
            );
            profile.insert("type", json!("local"));
            profile.insert("root", json!(root));
            ("local".to_string(), profile)
        }
        Some(CallRecordConfig::S3 {
            vendor,
            bucket,
            region,
            access_key,
            secret_key,
            endpoint,
            root,
            with_media,
            keep_media_copy,
        }) => {
            let mut profile = Profile::new(
                "callrecord-s3",
                "Call recordings",
                format!("Uploading call detail records to S3 bucket {}", bucket),
            );
            let vendor_value = serde_json::to_value(vendor)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("{:?}", vendor).to_lowercase());
            profile.insert("type", json!("s3"));
            profile.insert("vendor", json!(vendor_value));
            profile.insert("bucket", json!(bucket));
            profile.insert("region", json!(region));
            profile.insert("endpoint", json!(endpoint));
            profile.insert("root", json!(root));
            profile.insert("access_key", json!(mask_basic(access_key)));
            profile.insert("secret_key", json!(mask_basic(secret_key)));
            if let Some(flag) = with_media {
                profile.insert("with_media", json!(flag));
            }
            if let Some(flag) = keep_media_copy {
                profile.insert("keep_media_copy", json!(flag));
            }
            ("s3".to_string(), profile)
        }
        Some(CallRecordConfig::Http {
            url,
            headers,
            with_media,
            keep_media_copy,
        }) => {
            let mut profile = Profile::new(
                "callrecord-http",
                "Call recordings",
                "Streaming call detail records to HTTP endpoint",
            );
            profile.insert("type", json!("http"));
            profile.insert("url", json!(url));
            if let Some(headers) = headers {
                profile.insert("headers", json!(headers));
            }
            if let Some(flag) = with_media {
                profile.insert("with_media", json!(flag));
            }
            if let Some(flag) = keep_media_copy {
                profile.insert("keep_media_copy", json!(flag));
            }
            ("http".to_string(), profile)
        }
        Some(CallRecordConfig::Database {
            database_url,
            table_name,
        }) => {
            let mut profile = Profile::new(
                "callrecord-database",
                "Call recordings",
                "Storing call detail records in database",
            );
            profile.insert("type", json!("database"));
            if let Some(url) = database_url {
                profile.insert("database_url", json!(url));
            }
            profile.insert("table_name", json!(table_name));
            ("database".to_string(), profile)
        }
        None => {
            let mut profile = Profile::new(
                "callrecord-local",
                "Call recordings",
                format!("Storing call detail records on {}", recorder_path),
            );
            profile.insert("type", json!("local"));
            profile.insert("root", json!(&recorder_path));
            ("local".to_string(), profile)
        }
    };

    let mut spool_profile = Profile::new(
        "spool-paths",
        "Spool directories",
        "Server-side spool paths for recordings and media cache.",
    );
    spool_profile.insert("recorder_path", json!(&recorder_path));

    if let Some(policy) = config.recording.as_ref()
        && let Ok(policy_value) = serde_json::to_value(policy) {
            spool_profile.insert("recording", policy_value);
        }

    let active_profile_id = callrecord_profile.id.clone();
    let active_description = callrecord_profile.description.clone();
    let storage_mode = mode.clone();

    let mut storage_meta = serde_json::Map::new();
    storage_meta.insert("mode".to_string(), json!(storage_mode));
    storage_meta.insert("active_profile".to_string(), json!(active_profile_id));
    storage_meta.insert("description".to_string(), json!(active_description));
    storage_meta.insert("recorder_path".to_string(), json!(&recorder_path));
    storage_meta.insert(
        "recording".to_string(),
        config
            .recording
            .as_ref()
            .and_then(|policy| serde_json::to_value(policy).ok())
            .unwrap_or(JsonValue::Null),
    );

    let profiles = vec![callrecord_profile.into_json(), spool_profile.into_json()];

    (JsonValue::Object(storage_meta), profiles)
}

async fn query_departments(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<forms::ListQuery<QueryDepartmentFilters>>,
) -> Response {
    if !state.has_permission(&user, "departments", "read").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let db = state.db();
    let mut selector = DepartmentEntity::find().order_by_asc(DepartmentColumn::Name);
    if let Some(filters) = payload.filters.as_ref()
        && let Some(keyword) = filters
            .q
            .as_ref()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            let pattern = format!("%{}%", keyword);
            selector = selector.filter(
                Condition::any()
                    .add(DepartmentColumn::Name.like(pattern.clone()))
                    .add(DepartmentColumn::DisplayLabel.like(pattern.clone()))
                    .add(DepartmentColumn::Slug.like(pattern)),
            );
        }

    let paginator = selector.paginate(db, payload.normalize().1);
    let pagination = match forms::paginate(paginator, &payload).await {
        Ok(pagination) => pagination,
        Err(err) => {
            warn!("failed to query departments: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response();
        }
    };

    let forms::Pagination {
        items,
        current_page,
        per_page,
        total_items,
        total_pages,
        has_prev,
        has_next,
    } = pagination;

    Json(json!({
        "page": current_page,
        "per_page": per_page,
        "total_items": total_items,
        "total_pages": total_pages,
        "has_prev": has_prev,
        "has_next": has_next,
        "items": items,
    }))
    .into_response()
}

async fn get_department(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "departments", "read").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    match DepartmentEntity::find_by_id(id).one(state.db()).await {
        Ok(Some(model)) => Json(model).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "Department not found"})),
        )
            .into_response(),
        Err(err) => {
            warn!("failed to load department {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn create_department(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<DepartmentPayload>,
) -> Response {
    if !state.has_permission(&user, "departments", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let name = payload.name.trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Department name is required"})),
        )
            .into_response();
    }

    let now = Utc::now();
    let mut active = DepartmentActiveModel {
        name: Set(name.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    active.display_label = Set(normalize_opt_string(payload.display_label));
    active.slug = Set(normalize_opt_string(payload.slug));
    active.description = Set(normalize_opt_string(payload.description));
    active.color = Set(normalize_opt_string(payload.color));
    active.manager_contact = Set(normalize_opt_string(payload.manager_contact));
    active.metadata = Set(payload.metadata);

    match active.insert(state.db()).await {
        Ok(model) => (
            StatusCode::CREATED,
            Json(json!({"status": "ok", "id": model.id})),
        )
            .into_response(),
        Err(err) => {
            warn!("failed to create department: {}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn update_department(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<DepartmentPayload>,
) -> Response {
    if !state.has_permission(&user, "departments", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let model = match DepartmentEntity::find_by_id(id).one(state.db()).await {
        Ok(Some(model)) => model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "Department not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load department {} for update: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response();
        }
    };

    let mut active: DepartmentActiveModel = model.into();
    let name = payload.name.trim();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Department name is required"})),
        )
            .into_response();
    }
    active.name = Set(name.to_string());
    active.display_label = Set(normalize_opt_string(payload.display_label));
    active.slug = Set(normalize_opt_string(payload.slug));
    active.description = Set(normalize_opt_string(payload.description));
    active.color = Set(normalize_opt_string(payload.color));
    active.manager_contact = Set(normalize_opt_string(payload.manager_contact));
    active.metadata = Set(payload.metadata);
    active.updated_at = Set(Utc::now());

    match active.update(state.db()).await {
        Ok(_) => Json(json!({"status": "ok"})).into_response(),
        Err(err) => {
            warn!("failed to update department {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn delete_department(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "departments", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let model = match DepartmentEntity::find_by_id(id).one(state.db()).await {
        Ok(Some(model)) => model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "Department not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load department {} for delete: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response();
        }
    };

    let active: DepartmentActiveModel = model.into();
    match active.delete(state.db()).await {
        Ok(_) => Json(json!({"status": "ok"})).into_response(),
        Err(err) => {
            warn!("failed to delete department {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn query_users(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(query): Json<forms::ListQuery<QueryUserFilters>>,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = state.db();
    let mut selector = UserEntity::find().order_by_asc(UserColumn::Username);
    if let Some(filters) = query.filters.as_ref() {
        if let Some(keyword) = filters
            .q
            .as_ref()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            let pattern = format!("%{}%", keyword);
            selector = selector.filter(
                Condition::any()
                    .add(UserColumn::Email.like(pattern.clone()))
                    .add(UserColumn::Username.like(pattern)),
            );
        }
        if let Some(active_only) = filters.active
            && active_only {
                selector = selector.filter(UserColumn::IsActive.eq(true));
            }
    }

    let paginator = selector.paginate(db, query.normalize().1);
    let pagination = match forms::paginate(paginator, &query).await {
        Ok(pagination) => pagination,
        Err(err) => {
            warn!("failed to query users: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response();
        }
    };

    let forms::Pagination {
        items,
        current_page,
        per_page,
        total_items,
        total_pages,
        has_prev,
        has_next,
    } = pagination;

    let view_items: Vec<UserView> = items.into_iter().map(UserView::from).collect();

    Json(json!({
        "page": current_page,
        "per_page": per_page,
        "total_items": total_items,
        "total_pages": total_pages,
        "has_prev": has_prev,
        "has_next": has_next,
        "items": view_items,
    }))
    .into_response()
}

async fn get_user(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    match UserEntity::find_by_id(id).one(state.db()).await {
        Ok(Some(model)) => Json(UserView::from(model)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "User not found"})),
        )
            .into_response(),
        Err(err) => {
            warn!("failed to load user {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn create_user(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<UserPayload>,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let email = payload.email.trim();
    let username = payload.username.trim();
    if email.is_empty() || username.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Email and username are required"})),
        )
            .into_response();
    }
    let password = match payload
        .password
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        Some(password) => password.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"message": "Password is required"})),
            )
                .into_response();
        }
    };

    if email_exists(state.db(), email, None).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Email already in use"})),
        )
            .into_response();
    }
    if username_exists(state.db(), username, None).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Username already in use"})),
        )
            .into_response();
    }

    let now = Utc::now();
    let hashed = match hash_password(&password) {
        Ok(hash) => hash,
        Err(err) => {
            warn!("failed to hash password: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": "Failed to hash password"})),
            )
                .into_response();
        }
    };

    let active = UserActiveModel {
        email: Set(email.to_lowercase()),
        username: Set(username.to_string()),
        password_hash: Set(hashed),
        created_at: Set(now),
        updated_at: Set(now),
        is_active: Set(payload.is_active.unwrap_or(true)),
        is_staff: Set(payload.is_staff.unwrap_or(false)),
        is_superuser: Set(payload.is_superuser.unwrap_or(false)),
        ..Default::default()
    };

    match active.insert(state.db()).await {
        Ok(model) => (
            StatusCode::CREATED,
            Json(json!({"status": "ok", "id": model.id})),
        )
            .into_response(),
        Err(err) => {
            warn!("failed to create user: {}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn update_user(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<UserPayload>,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let model = match UserEntity::find_by_id(id).one(state.db()).await {
        Ok(Some(model)) => model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "User not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load user {} for update: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response();
        }
    };

    let email = payload.email.trim();
    let username = payload.username.trim();
    if email.is_empty() || username.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Email and username are required"})),
        )
            .into_response();
    }

    if email_exists(state.db(), email, Some(id)).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Email already in use"})),
        )
            .into_response();
    }
    if username_exists(state.db(), username, Some(id)).await {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "Username already in use"})),
        )
            .into_response();
    }

    let mut active: UserActiveModel = model.into();
    active.email = Set(email.to_lowercase());
    active.username = Set(username.to_string());
    active.is_active = Set(payload.is_active.unwrap_or(true));
    active.is_staff = Set(payload.is_staff.unwrap_or(false));
    active.is_superuser = Set(payload.is_superuser.unwrap_or(false));
    active.updated_at = Set(Utc::now());

    if let Some(password) = payload
        .password
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        match hash_password(password) {
            Ok(hash) => active.password_hash = Set(hash),
            Err(err) => {
                warn!("failed to hash password: {}", err);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"message": "Failed to hash password"})),
                )
                    .into_response();
            }
        }
    }

    match active.update(state.db()).await {
        Ok(_) => Json(json!({"status": "ok"})).into_response(),
        Err(err) => {
            warn!("failed to update user {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn delete_user(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let model = match UserEntity::find_by_id(id).one(state.db()).await {
        Ok(Some(model)) => model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "User not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load user {} for delete: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response();
        }
    };

    let active: UserActiveModel = model.into();
    match active.delete(state.db()).await {
        Ok(_) => Json(json!({"status": "ok"})).into_response(),
        Err(err) => {
            warn!("failed to delete user {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": err.to_string()})),
            )
                .into_response()
        }
    }
}

async fn email_exists(db: &sea_orm::DatabaseConnection, email: &str, exclude: Option<i64>) -> bool {
    let mut selector = UserEntity::find().filter(UserColumn::Email.eq(email));
    if let Some(id) = exclude {
        selector = selector.filter(UserColumn::Id.ne(id));
    }
    selector.count(db).await.unwrap_or(0) > 0
}

async fn username_exists(
    db: &sea_orm::DatabaseConnection,
    username: &str,
    exclude: Option<i64>,
) -> bool {
    let mut selector = UserEntity::find().filter(UserColumn::Username.eq(username));
    if let Some(id) = exclude {
        selector = selector.filter(UserColumn::Id.ne(id));
    }
    selector.count(db).await.unwrap_or(0) > 0
}

fn normalize_opt_string(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
}

fn human_duration(duration: Duration) -> String {
    let total = duration.num_seconds().max(0);
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{}d", days));
    }
    if hours > 0 {
        parts.push(format!("{}h", hours));
    }
    if minutes > 0 {
        parts.push(format!("{}m", minutes));
    }
    if seconds > 0 && parts.is_empty() {
        parts.push(format!("{}s", seconds));
    }

    if parts.is_empty() {
        "0s".to_string()
    } else {
        parts.join(" ")
    }
}

fn mask_basic(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 4 {
        return "****".to_string();
    }
    let mut masked = String::new();
    masked.extend(&chars[..2]);
    masked.push_str("****");
    masked.extend(&chars[chars.len() - 2..]);
    masked
}

fn summarize_callrecord(config: Option<&CallRecordConfig>) -> Option<JsonValue> {
    match config? {
        CallRecordConfig::Local { root } => Some(json!({
            "label": "Call record storage",
            "value": format!("Local ({})", root),
        })),
        CallRecordConfig::S3 {
            bucket,
            region,
            endpoint,
            ..
        } => Some(json!({
            "label": "Call record storage",
            "value": format!("S3 bucket {} ({})", bucket, region),
            "hint": endpoint,
        })),
        CallRecordConfig::Http { url, .. } => Some(json!({
            "label": "Call record storage",
            "value": format!("HTTP {}", url),
        })),
        CallRecordConfig::Database {
            database_url,
            table_name,
        } => Some(json!({
            "label": "Call record storage",
            "value": format!("Database ({})", table_name),
            "hint": database_url.as_deref().unwrap_or("default"),
        })),
    }
}

fn build_port_list(proxy_cfg: &ProxyConfig) -> Vec<JsonValue> {
    let mut ports = Vec::new();
    if let Some(port) = proxy_cfg.udp_port {
        ports.push(json!({ "label": "UDP", "value": port }));
    }
    if let Some(port) = proxy_cfg.tcp_port {
        ports.push(json!({ "label": "TCP", "value": port }));
    }
    if let Some(port) = proxy_cfg.tls_port {
        ports.push(json!({ "label": "TLS", "value": port }));
    }
    if let Some(port) = proxy_cfg.ws_port {
        ports.push(json!({ "label": "WS", "value": port }));
    }
    ports
}

#[derive(Debug, Deserialize)]
pub(crate) struct PlatformSettingsPayload {
    #[serde(default)]
    log_level: Option<Option<String>>,
    #[serde(default)]
    log_file: Option<Option<String>>,
    #[serde(default)]
    external_ip: Option<Option<String>>,
    #[serde(default)]
    rtp_start_port: Option<Option<u16>>,
    #[serde(default)]
    rtp_end_port: Option<Option<u16>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TestStoragePayload {
    pub vendor: crate::storage::S3Vendor,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub endpoint: Option<String>,
    pub root: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StorageSettingsPayload {
    #[serde(default)]
    recorder_path: Option<Option<String>>,
    #[serde(default)]
    media_cache_path: Option<Option<String>>,
    #[serde(default)]
    recorder_format: Option<Option<String>>,
    #[serde(default)]
    callrecord: Option<Option<CallRecordStoragePayload>>,
    #[serde(default)]
    recording_policy: Option<Option<RecordingPolicyPayload>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum CallRecordStoragePayload {
    Disabled,
    Local {
        #[serde(default)]
        root: Option<String>,
    },
    S3 {
        vendor: crate::storage::S3Vendor,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        root: Option<String>,
        #[serde(default)]
        with_media: Option<bool>,
        #[serde(default)]
        keep_media_copy: Option<bool>,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordingPolicyPayload {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    directions: Option<Vec<String>>,
    #[serde(default)]
    caller_allow: Option<Vec<String>>,
    #[serde(default)]
    caller_deny: Option<Vec<String>>,
    #[serde(default)]
    callee_allow: Option<Vec<String>>,
    #[serde(default)]
    callee_deny: Option<Vec<String>>,
    #[serde(default)]
    auto_start: Option<bool>,
    #[serde(default)]
    filename_pattern: Option<Option<String>>,
    #[serde(default)]
    samplerate: Option<Option<u32>>,
    #[serde(default)]
    ptime: Option<Option<u32>>,
    #[serde(default)]
    path: Option<Option<String>>,
    #[serde(default)]
    format: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SecuritySettingsPayload {
    #[serde(default)]
    acl_rules: Option<Option<String>>,
}

/// GET /settings/config
///
/// Returns all effective configuration: the in-memory merged config (what the
/// server is actually running with) combined with the raw DB overrides so the
/// caller can see both the effective values and which keys have been overridden.
pub(crate) async fn get_effective_config(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "system", "read").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    // Load all DB overrides
    let db_overrides = match system_config::Model::get_all(&db).await {
        Ok(rows) => rows,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to load config overrides: {e}"),
            );
        }
    };

    // Build a map of key → {value, is_override, updated_at}
    let overrides_map: serde_json::Value = serde_json::Value::Object(
        db_overrides
            .iter()
            .map(|row| {
                let val: serde_json::Value = serde_json::from_str(&row.value)
                    .unwrap_or(serde_json::Value::String(row.value.clone()));
                (
                    row.key.clone(),
                    json!({
                        "value": val,
                        "is_override": row.is_override,
                        "updated_at": row.updated_at.to_rfc3339(),
                    }),
                )
            })
            .collect(),
    );

    // Include the effective running config (what server booted with).
    // Expose enough of each section that the UI can render "current running
    // values" next to each tab without needing to call multiple endpoints.
    let effective = if let Some(app_state) = state.app_state() {
        let config = app_state.config();
        let proxy_json = serde_json::to_value(&config.proxy).unwrap_or(JsonValue::Null);
        let rwi_json = serde_json::to_value(&config.rwi).unwrap_or(JsonValue::Null);
        let recording_json =
            serde_json::to_value(&config.recording).unwrap_or(JsonValue::Null);
        let callrecord_json =
            serde_json::to_value(&config.callrecord).unwrap_or(JsonValue::Null);
        let sipflow_json =
            serde_json::to_value(&config.sipflow).unwrap_or(JsonValue::Null);
        json!({
            "http_addr": config.http_addr,
            "external_ip": config.external_ip,
            "log_level": config.log_level,
            "log_file": config.log_file,
            "database_url": config.database_url,
            "rtp_start_port": config.rtp_start_port,
            "rtp_end_port": config.rtp_end_port,
            "recording": {
                "path": config.recorder_path(),
                "policy": recording_json,
            },
            "callrecord": callrecord_json,
            "proxy": proxy_json,
            "rwi": rwi_json,
            "sipflow": sipflow_json,
        })
    } else {
        json!(null)
    };

    Json(json!({
        "status": "ok",
        "effective_config": effective,
        "db_overrides": overrides_map,
        "override_count": db_overrides.len(),
        "note": "effective_config shows running values; db_overrides shows what is stored in DB. Restart applies all overrides."
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfigEntryPayload {
    key: String,
    /// Raw JSON string as it will be stored (e.g. "\"debug\"", "5060", "true",
    /// "[\"a\",\"b\"]"). The backend validates it by parsing as JSON.
    value: String,
    #[serde(default)]
    is_override: bool,
}

/// PATCH /settings/config/entry — upsert a single system_config row.
pub(crate) async fn upsert_config_entry(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<ConfigEntryPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let key = payload.key.trim();
    if key.is_empty() {
        return json_error(StatusCode::UNPROCESSABLE_ENTITY, "key must not be empty");
    }

    // Validate the value is valid JSON so the merge step won't silently skip it
    if serde_json::from_str::<serde_json::Value>(&payload.value).is_err() {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "value must be valid JSON (e.g. \"debug\" for strings, 5060 for numbers)",
        );
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    if let Err(e) = system_config::Model::upsert(&db, key, &payload.value, payload.is_override).await {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to save entry: {e}"),
        );
    }

    Json(json!({
        "status": "ok",
        "requires_restart": true,
        "message": "Entry saved. Restart RustPBX to apply.",
        "key": key,
    }))
    .into_response()
}

/// DELETE /settings/config/entry/{key} — remove a system_config row.
pub(crate) async fn delete_config_entry(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    AxumPath(key): AxumPath<String>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    use sea_orm::EntityTrait;
    match system_config::Entity::delete_by_id(key.clone()).exec(&db).await {
        Ok(res) => Json(json!({
            "status": "ok",
            "deleted": res.rows_affected,
            "key": key,
            "requires_restart": true,
        }))
        .into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to delete entry: {e}"),
        ),
    }
}

pub(crate) async fn update_platform_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<PlatformSettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    // Load base config for validation only (not written back to disk)
    let config_path = match get_config_path(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut doc = match load_document(&config_path) {
        Ok(d) => d,
        Err(r) => return r,
    };

    let mut overrides: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let mut modified = false;

    if let Some(level_opt) = payload.log_level {
        if let Some(level) = normalize_opt_string(level_opt) {
            doc["log_level"] = value(level.clone());
            overrides.push(("log_level", serde_json::json!(level)));
        } else {
            doc.remove("log_level");
        }
        modified = true;
    }

    if let Some(file_opt) = payload.log_file {
        if let Some(path) = normalize_opt_string(file_opt) {
            doc["log_file"] = value(path.clone());
            overrides.push(("log_file", serde_json::json!(path)));
        } else {
            doc.remove("log_file");
        }
        modified = true;
    }

    if let Some(ext_opt) = payload.external_ip {
        if let Some(ip) = normalize_opt_string(ext_opt) {
            doc["external_ip"] = value(ip.clone());
            overrides.push(("external_ip", serde_json::json!(ip)));
        } else {
            doc.remove("external_ip");
        }
        modified = true;
    }

    if let Some(start_opt) = payload.rtp_start_port {
        if let Some(port) = start_opt {
            if port == 0 {
                return json_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "rtp_start_port must be greater than 0",
                );
            }
            doc["rtp_start_port"] = value(i64::from(port));
            overrides.push(("rtp_start_port", serde_json::json!(port)));
        } else {
            doc.remove("rtp_start_port");
        }
        modified = true;
    }

    if let Some(end_opt) = payload.rtp_end_port {
        if let Some(port) = end_opt {
            if port == 0 {
                return json_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "rtp_end_port must be greater than 0",
                );
            }
            doc["rtp_end_port"] = value(i64::from(port));
            overrides.push(("rtp_end_port", serde_json::json!(port)));
        } else {
            doc.remove("rtp_end_port");
        }
        modified = true;
    }

    // Validate the merged document as a Config
    let doc_text = doc.to_string();
    let config = match parse_config_from_str(&doc_text) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };

    if let (Some(start), Some(end)) = (config.rtp_start_port, config.rtp_end_port)
        && start > end {
            return json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "rtp_start_port must be less than or equal to rtp_end_port",
            );
        }

    // Persist to DB (not to disk)
    if modified {
        for (key, val) in overrides {
            let is_override = key == "external_ip"; // manual IP = skip auto-detection
            if let Err(e) =
                system_config::Model::upsert(&db, key, &val.to_string(), is_override).await
            {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to save setting '{key}': {e}"),
                );
            }
        }
    }

    Json(json!({
        "status": "ok",
        "requires_restart": true,
        "message": "Platform settings saved. Restart RustPBX to apply changes.",
        "platform": {
            "log_level": config.log_level,
            "log_file": config.log_file,
        },
        "rtp": {
            "external_ip": config.external_ip,
            "start_port": config.rtp_start_port,
            "end_port": config.rtp_end_port,
        }
    }))
    .into_response()
}

pub(crate) async fn update_proxy_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<ProxySettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }

    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    let config_path = match get_config_path(&state) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mut doc = match load_document(&config_path) {
        Ok(d) => d,
        Err(r) => return r,
    };

    let mut overrides: Vec<(&'static str, serde_json::Value)> = Vec::new();
    let mut modified = false;
    let table = ensure_table_mut(&mut doc, "proxy");

    if let Some(realms) = payload.realms {
        set_string_array(table, "realms", realms.clone());
        overrides.push(("proxy.realms", serde_json::json!(realms)));
        modified = true;
    }

    if let Some(webhook) = payload.locator_webhook {
        let toml_s = toml::to_string(&webhook).unwrap_or_default();
        if let Ok(new_doc) = toml_s.parse::<DocumentMut>() {
            table["locator_webhook"] = new_doc.as_item().clone();
        }
        overrides.push(("proxy.locator_webhook", serde_json::to_value(&webhook).unwrap_or_default()));
        modified = true;
    }

    if let Some(backends) = payload.user_backends {
        let toml_s = toml::to_string(&json!({ "b": backends })).unwrap_or_default();
        if let Ok(new_doc) = toml_s.parse::<DocumentMut>() {
            table["user_backends"] = new_doc["b"].clone();
        }
        overrides.push(("proxy.user_backends", serde_json::to_value(&backends).unwrap_or_default()));
        modified = true;
    }

    if let Some(router) = payload.http_router {
        let toml_s = toml::to_string(&router).unwrap_or_default();
        if let Ok(new_doc) = toml_s.parse::<DocumentMut>() {
            table["http_router"] = new_doc.as_item().clone();
        }
        overrides.push(("proxy.http_router", serde_json::to_value(&router).unwrap_or_default()));
        modified = true;
    }

    if modified {
        let doc_text = doc.to_string();
        if let Err(resp) = parse_config_from_str(&doc_text) {
            return resp;
        }
        for (key, val) in overrides {
            if let Err(e) =
                system_config::Model::upsert(&db, key, &val.to_string(), false).await
            {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to save setting '{key}': {e}"),
                );
            }
        }
    }

    Json(json!({
        "status": "ok",
        "message": "Proxy settings saved. Restart required to apply.",
    }))
    .into_response()
}

pub(crate) async fn update_storage_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<StorageSettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let config_path = match get_config_path(&state) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    let mut doc = match load_document(&config_path) {
        Ok(doc) => doc,
        Err(resp) => return resp,
    };

    // DB ops collected while mutating `doc` (which is used for Config validation).
    // Some(v) = upsert, None = delete the row so base config.toml value takes effect.
    let mut ops: Vec<(String, Option<serde_json::Value>)> = Vec::new();
    let mut modified = false;

    if let Some(path_opt) = payload.recorder_path {
        {
            let table = ensure_table_mut(&mut doc, "recording");
            match normalize_opt_string(path_opt) {
                Some(path) => {
                    table["path"] = value(path.clone());
                    ops.push(("recording.path".into(), Some(json!(path))));
                }
                None => {
                    table.remove("path");
                    ops.push(("recording.path".into(), None));
                }
            }
        }
        doc.remove("recorder_path");
        modified = true;
    }

    if let Some(cache_opt) = payload.media_cache_path {
        match normalize_opt_string(cache_opt) {
            Some(path) => {
                doc["media_cache_path"] = value(path.clone());
                ops.push(("media_cache_path".into(), Some(json!(path))));
            }
            None => {
                doc.remove("media_cache_path");
                ops.push(("media_cache_path".into(), None));
            }
        }
        modified = true;
    }

    // Signal that the entire `callrecord.*` key space should be wiped before
    // upserting the new values. Used so stale fields from the previous mode
    // (e.g. local→s3) or seeded base-config values don't leak back in via
    // config_merge::seed_missing_from_doc.
    let mut clear_callrecord = false;

    if let Some(callrecord_opt) = payload.callrecord {
        clear_callrecord = true;
        match callrecord_opt {
            Some(CallRecordStoragePayload::Disabled) | None => {
                doc.remove("callrecord");
                // ops entries are a no-op for disabled: the blanket wipe below
                // is the whole effect.
            }
            Some(CallRecordStoragePayload::Local { root }) => {
                let Some(root_path) = normalize_opt_string(root) else {
                    return json_error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "callrecord.local.root is required",
                    );
                };
                let mut table = Table::new();
                table["type"] = value("local");
                table["root"] = value(root_path.clone());
                doc["callrecord"] = Item::Table(table);
                ops.push(("callrecord.type".into(), Some(json!("local"))));
                ops.push(("callrecord.root".into(), Some(json!(root_path))));
            }
            Some(CallRecordStoragePayload::S3 {
                vendor,
                bucket,
                region,
                access_key,
                secret_key,
                endpoint,
                root,
                with_media,
                keep_media_copy,
            }) => {
                let endpoint = endpoint.unwrap_or_default();
                let root = root.unwrap_or_default();
                let vendor_str = serde_json::to_value(&vendor)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "aws".to_string());
                let mut table = Table::new();
                table["type"] = value("s3");
                table["vendor"] = value(vendor_str.clone());
                table["bucket"] = value(bucket.clone());
                table["region"] = value(region.clone());
                table["access_key"] = value(access_key.clone());
                table["secret_key"] = value(secret_key.clone());
                table["endpoint"] = value(endpoint.clone());
                table["root"] = value(root.clone());
                if let Some(b) = with_media {
                    table["with_media"] = value(b);
                }
                if let Some(b) = keep_media_copy {
                    table["keep_media_copy"] = value(b);
                }
                doc["callrecord"] = Item::Table(table);

                ops.push(("callrecord.type".into(), Some(json!("s3"))));
                ops.push(("callrecord.vendor".into(), Some(json!(vendor_str))));
                ops.push(("callrecord.bucket".into(), Some(json!(bucket))));
                ops.push(("callrecord.region".into(), Some(json!(region))));
                ops.push(("callrecord.access_key".into(), Some(json!(access_key))));
                ops.push(("callrecord.secret_key".into(), Some(json!(secret_key))));
                ops.push(("callrecord.endpoint".into(), Some(json!(endpoint))));
                ops.push(("callrecord.root".into(), Some(json!(root))));
                if let Some(b) = with_media {
                    ops.push(("callrecord.with_media".into(), Some(json!(b))));
                }
                if let Some(b) = keep_media_copy {
                    ops.push(("callrecord.keep_media_copy".into(), Some(json!(b))));
                }
            }
        }
        modified = true;
    }

    // All individual recording.* keys managed by recording_policy.
    const RECORDING_KEYS: &[&str] = &[
        "recording.enabled",
        "recording.auto_start",
        "recording.directions",
        "recording.caller_allow",
        "recording.caller_deny",
        "recording.callee_allow",
        "recording.callee_deny",
        "recording.filename_pattern",
        "recording.samplerate",
        "recording.ptime",
        "recording.path",
        "recording.format",
    ];

    if let Some(policy_opt) = payload.recording_policy {
        match policy_opt {
            Some(policy_payload) => {
                let mut table = Table::new();
                let enabled = policy_payload.enabled.unwrap_or(false);
                table["enabled"] = value(enabled);
                ops.push(("recording.enabled".into(), Some(json!(enabled))));

                if let Some(directions) = policy_payload.directions {
                    if directions.is_empty() {
                        table.remove("directions");
                        ops.push(("recording.directions".into(), None));
                    } else {
                        set_string_array(&mut table, "directions", directions.clone());
                        ops.push(("recording.directions".into(), Some(json!(directions))));
                    }
                }

                if let Some(allow) = policy_payload.caller_allow {
                    if allow.is_empty() {
                        table.remove("caller_allow");
                        ops.push(("recording.caller_allow".into(), None));
                    } else {
                        set_string_array(&mut table, "caller_allow", allow.clone());
                        ops.push(("recording.caller_allow".into(), Some(json!(allow))));
                    }
                }

                if let Some(deny) = policy_payload.caller_deny {
                    if deny.is_empty() {
                        table.remove("caller_deny");
                        ops.push(("recording.caller_deny".into(), None));
                    } else {
                        set_string_array(&mut table, "caller_deny", deny.clone());
                        ops.push(("recording.caller_deny".into(), Some(json!(deny))));
                    }
                }

                if let Some(allow) = policy_payload.callee_allow {
                    if allow.is_empty() {
                        table.remove("callee_allow");
                        ops.push(("recording.callee_allow".into(), None));
                    } else {
                        set_string_array(&mut table, "callee_allow", allow.clone());
                        ops.push(("recording.callee_allow".into(), Some(json!(allow))));
                    }
                }

                if let Some(deny) = policy_payload.callee_deny {
                    if deny.is_empty() {
                        table.remove("callee_deny");
                        ops.push(("recording.callee_deny".into(), None));
                    } else {
                        set_string_array(&mut table, "callee_deny", deny.clone());
                        ops.push(("recording.callee_deny".into(), Some(json!(deny))));
                    }
                }

                if let Some(auto_start) = policy_payload.auto_start {
                    table["auto_start"] = value(auto_start);
                    ops.push(("recording.auto_start".into(), Some(json!(auto_start))));
                }

                match policy_payload.filename_pattern {
                    Some(Some(pattern)) => {
                        let trimmed = pattern.trim();
                        if trimmed.is_empty() {
                            table.remove("filename_pattern");
                            ops.push(("recording.filename_pattern".into(), None));
                        } else {
                            table["filename_pattern"] = value(trimmed);
                            ops.push((
                                "recording.filename_pattern".into(),
                                Some(json!(trimmed)),
                            ));
                        }
                    }
                    Some(None) => {
                        table.remove("filename_pattern");
                        ops.push(("recording.filename_pattern".into(), None));
                    }
                    None => {}
                }

                match policy_payload.samplerate {
                    Some(Some(rate)) => {
                        table["samplerate"] = value(i64::from(rate));
                        ops.push(("recording.samplerate".into(), Some(json!(rate))));
                    }
                    Some(None) => {
                        table.remove("samplerate");
                        ops.push(("recording.samplerate".into(), None));
                    }
                    None => {}
                }

                match policy_payload.ptime {
                    Some(Some(ptime)) => {
                        table["ptime"] = value(i64::from(ptime));
                        ops.push(("recording.ptime".into(), Some(json!(ptime))));
                    }
                    Some(None) => {
                        table.remove("ptime");
                        ops.push(("recording.ptime".into(), None));
                    }
                    None => {}
                }

                if let Some(path_opt) = policy_payload.path {
                    match normalize_opt_string(path_opt) {
                        Some(path) => {
                            table["path"] = value(path.clone());
                            ops.push(("recording.path".into(), Some(json!(path))));
                        }
                        None => {
                            table.remove("path");
                            ops.push(("recording.path".into(), None));
                        }
                    }
                }

                if let Some(format_opt) = policy_payload.format {
                    match format_opt {
                        Some(format_value) => {
                            let normalized = format_value.trim().to_ascii_lowercase();
                            if normalized.is_empty() {
                                table.remove("format");
                                ops.push(("recording.format".into(), None));
                            } else if normalized == "wav" || normalized == "ogg" {
                                table["format"] = value(normalized.clone());
                                ops.push((
                                    "recording.format".into(),
                                    Some(json!(normalized)),
                                ));
                            } else {
                                return json_error(
                                    StatusCode::UNPROCESSABLE_ENTITY,
                                    "recording.format must be either 'wav' or 'ogg'",
                                );
                            }
                        }
                        None => {
                            table.remove("format");
                            ops.push(("recording.format".into(), None));
                        }
                    }
                }

                doc["recording"] = Item::Table(table);
            }
            None => {
                doc.remove("recording");
                for key in RECORDING_KEYS {
                    ops.push((key.to_string(), None));
                }
            }
        }
        modified = true;
    }

    if let Some(format_opt) = payload.recorder_format {
        {
            let table = ensure_table_mut(&mut doc, "recording");
            match format_opt {
                Some(format_value) => {
                    let normalized = format_value.trim().to_ascii_lowercase();
                    if normalized.is_empty() {
                        table.remove("format");
                        ops.push(("recording.format".into(), None));
                    } else if normalized == "wav" {
                        table["format"] = value(normalized.clone());
                        ops.push(("recording.format".into(), Some(json!(normalized))));
                    } else {
                        return json_error(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "recorder_format must be 'wav'",
                        );
                    }
                }
                None => {
                    table.remove("format");
                    ops.push(("recording.format".into(), None));
                }
            }
        }
        doc.remove("recorder_format");
        modified = true;
    }

    // Validate the mutated doc parses as a Config before touching the DB.
    let doc_text = doc.to_string();
    let config = match parse_config_from_str(&doc_text) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };

    if modified {
        let db = match get_db(&state) {
            Ok(db) => db,
            Err(r) => return r,
        };
        use sea_orm::EntityTrait;
        // Wipe the entire `callrecord.*` key space before writing the new
        // set. Without this, fields from the previous mode (e.g. local→s3)
        // or seeded base-config values would leak back in on the next
        // startup via seed_missing_from_doc and clobber the current mode.
        if clear_callrecord {
            use sea_orm::{ColumnTrait, QueryFilter};
            if let Err(e) = system_config::Entity::delete_many()
                .filter(system_config::Column::Key.starts_with("callrecord."))
                .exec(&db)
                .await
            {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to clear stale callrecord.* rows: {e}"),
                );
            }
            // Also remove any legacy top-level `callrecord` row left over
            // from the previous (object-blob) storage format.
            let _ = system_config::Entity::delete_by_id("callrecord".to_string())
                .exec(&db)
                .await;
        }
        for (key, val_opt) in ops {
            match val_opt {
                Some(v) => {
                    if let Err(e) =
                        system_config::Model::upsert(&db, &key, &v.to_string(), false).await
                    {
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to save setting '{key}': {e}"),
                        );
                    }
                }
                None => {
                    if let Err(e) =
                        system_config::Entity::delete_by_id(key.clone()).exec(&db).await
                    {
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to delete setting '{key}': {e}"),
                        );
                    }
                }
            }
        }
    }

    let (storage_meta, storage_profiles) = build_storage_profiles(&config);

    Json(json!({
        "status": "ok",
        "requires_restart": true,
        "message": "Storage settings saved. Restart RustPBX to apply changes.",
        "storage": storage_meta,
        "storage_profiles": storage_profiles,
    }))
    .into_response()
}

pub(crate) async fn update_security_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<SecuritySettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let config_path = match get_config_path(&state) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    let mut doc = match load_document(&config_path) {
        Ok(doc) => doc,
        Err(resp) => return resp,
    };

    let mut modified = false;

    if let Some(acl_opt) = payload.acl_rules {
        let table = ensure_table_mut(&mut doc, "proxy");
        match acl_opt {
            Some(raw) => {
                let rules = parse_lines_to_vec(&raw);
                if rules.is_empty() {
                    table.remove("acl_rules");
                } else {
                    set_string_array(table, "acl_rules", rules);
                }
            }
            None => {
                table.remove("acl_rules");
            }
        }
        modified = true;
    }

    let doc_text = doc.to_string();
    let config = match parse_config_from_str(&doc_text) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };

    if modified {
        let db = match get_db(&state) {
            Ok(db) => db,
            Err(r) => return r,
        };
        let rules = config.proxy.acl_rules.clone().unwrap_or_default();
        if let Err(e) = system_config::Model::upsert(
            &db,
            "proxy.acl_rules",
            &serde_json::to_string(&rules).unwrap_or_default(),
            false,
        )
        .await
        {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save acl_rules: {e}"),
            );
        }
    }

    let acl_rules = config.proxy.acl_rules.clone().unwrap_or_default();
    if let Some(app_state) = state.app_state() {
        let _ = app_state
            .sip_server()
            .inner
            .data_context
            .reload_acl_rules(false, Some(Arc::new(config.proxy.clone())));
    }

    Json(json!({
        "status": "ok",
        "requires_restart": false,
        "message": "Security settings saved and applied.",
        "security": {
            "acl_rules": acl_rules,
        }
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct RwiSettingsPayload {
    enabled: Option<bool>,
    max_connections: Option<usize>,
    max_calls_per_connection: Option<usize>,
    orphan_hold_secs: Option<u32>,
    originate_rate_limit: Option<usize>,
    tokens: Option<Vec<RwiTokenPayload>>,
    contexts: Option<Vec<RwiContextPayload>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RwiTokenPayload {
    token: String,
    scopes: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RwiContextPayload {
    name: String,
    no_answer_timeout_secs: Option<u32>,
    no_answer_action: Option<String>,
    no_answer_transfer_target: Option<String>,
}

const ALLOWED_TIMEZONES: &[&str] = &[
    "Asia/Kolkata",
    "UTC",
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "Europe/London",
    "Europe/Paris",
    "Europe/Berlin",
    "Asia/Dubai",
    "Asia/Singapore",
    "Asia/Tokyo",
    "Australia/Sydney",
];

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DisplaySettingsPayload {
    pub display_timezone: String,
}

pub(crate) async fn update_display_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<DisplaySettingsPayload>,
) -> Response {
    if !user.is_superuser && !state.has_permission(&user, "system", "write").await {
        return (StatusCode::FORBIDDEN, Json(json!({"message": "Permission denied"}))).into_response();
    }
    let tz = payload.display_timezone.trim();
    if !ALLOWED_TIMEZONES.contains(&tz) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": format!("Invalid timezone '{}'. Must be one of the supported IANA timezone names.", tz)})),
        )
            .into_response();
    }
    if let Err(e) = state.set_display_timezone(tz).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": format!("Failed to save timezone: {}", e)})),
        )
            .into_response();
    }
    Json(json!({"message": "Display settings saved", "display_timezone": state.display_timezone()}))
        .into_response()
}

pub(crate) async fn update_rwi_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<RwiSettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await
        && !state.has_permission(&user, "ami", "access").await
    {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let config_path = match get_config_path(&state) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    let mut doc = match load_document(&config_path) {
        Ok(doc) => doc,
        Err(resp) => return resp,
    };

    let mut modified = false;

    // Update [rwi] section
    let rwi_table = ensure_table_mut(&mut doc, "rwi");

    if let Some(enabled) = payload.enabled {
        rwi_table.insert("enabled", value(enabled));
        modified = true;
    }

    if let Some(max_connections) = payload.max_connections {
        rwi_table.insert("max_connections", value(max_connections as i64));
        modified = true;
    }

    if let Some(max_calls) = payload.max_calls_per_connection {
        rwi_table.insert("max_calls_per_connection", value(max_calls as i64));
        modified = true;
    }

    if let Some(orphan_hold) = payload.orphan_hold_secs {
        rwi_table.insert("orphan_hold_secs", value(orphan_hold as i64));
        modified = true;
    }

    if let Some(rate_limit) = payload.originate_rate_limit {
        rwi_table.insert("originate_rate_limit", value(rate_limit as i64));
        modified = true;
    }

    // Update tokens
    if let Some(tokens) = payload.tokens {
        #[derive(Serialize)]
        struct RwiTokensToml {
            tokens: Vec<RwiTokenPayload>,
        }

        let tokens_str = match toml::to_string(&RwiTokensToml { tokens }) {
            Ok(s) => s,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to serialize RWI tokens: {err}"),
                );
            }
        };
        let tokens_doc = match tokens_str.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid RWI tokens payload: {err}"),
                );
            }
        };
        let Some(tokens_item) = tokens_doc.get("tokens").cloned() else {
            return json_error(StatusCode::BAD_REQUEST, "Invalid RWI tokens payload");
        };
        rwi_table["tokens"] = tokens_item;
        modified = true;
    }

    // Update contexts
    if let Some(contexts) = payload.contexts {
        #[derive(Serialize)]
        struct RwiContextsToml {
            contexts: Vec<RwiContextPayload>,
        }

        let contexts_str = match toml::to_string(&RwiContextsToml { contexts }) {
            Ok(s) => s,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to serialize RWI contexts: {err}"),
                );
            }
        };
        let contexts_doc = match contexts_str.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid RWI contexts payload: {err}"),
                );
            }
        };
        let Some(contexts_item) = contexts_doc.get("contexts").cloned() else {
            return json_error(StatusCode::BAD_REQUEST, "Invalid RWI contexts payload");
        };
        rwi_table["contexts"] = contexts_item;
        modified = true;
    }

    let doc_text = doc.to_string();
    let config = match parse_config_from_str(&doc_text) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };

    if modified {
        let db = match get_db(&state) {
            Ok(db) => db,
            Err(r) => return r,
        };
        if let Err(e) = system_config::Model::upsert(
            &db,
            "rwi",
            &serde_json::to_string(&config.rwi).unwrap_or_default(),
            false,
        )
        .await
        {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to save rwi settings: {e}"),
            );
        }
    }

    let rwi_config = config.rwi.unwrap_or_default();
    Json(json!({
        "status": "ok",
        "requires_restart": true,
        "message": "RWI settings saved. Please restart the service for changes to take effect.",
        "rwi": rwi_config
    }))
    .into_response()
}

/// Get the DB connection from the console state, returning an error Response if unavailable.
fn get_db(state: &ConsoleState) -> Result<sea_orm::DatabaseConnection, Response> {
    state
        .app_state()
        .map(|s| s.db().clone())
        .ok_or_else(|| json_error(StatusCode::SERVICE_UNAVAILABLE, "Application state unavailable"))
}

#[cfg(feature = "commerce")]
#[derive(Debug, Deserialize)]
pub(crate) struct ClusterSettingsPayload {
    #[serde(default)]
    pub peers: Option<Vec<ClusterPeerPayload>>,
}

#[cfg(feature = "commerce")]
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ClusterPeerPayload {
    pub addr: String,
    pub sip_port: u16,
    pub ami_port: u16,
}

#[cfg(feature = "commerce")]
pub(crate) async fn update_cluster_settings(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<ClusterSettingsPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let config_path = match get_config_path(&state) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    let mut doc = match load_document(&config_path) {
        Ok(doc) => doc,
        Err(resp) => return resp,
    };

    let mut modified = false;

    if let Some(peers) = payload.peers {
        #[derive(Serialize)]
        struct ClusterToml {
            peers: Vec<ClusterPeerPayload>,
        }
        let peers_str = match toml::to_string(&ClusterToml { peers }) {
            Ok(s) => s,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to serialize cluster peers: {err}"),
                );
            }
        };
        let peers_doc = match peers_str.parse::<DocumentMut>() {
            Ok(doc) => doc,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid cluster peers payload: {err}"),
                );
            }
        };
        let cluster_table = ensure_table_mut(&mut doc, "cluster");
        let Some(peers_item) = peers_doc.get("peers").cloned() else {
            return json_error(StatusCode::BAD_REQUEST, "Invalid cluster peers payload");
        };
        cluster_table.insert("peers", peers_item);
        modified = true;
    }

    let doc_text = doc.to_string();
    let config = match parse_config_from_str(&doc_text) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };

    if modified
        && let Err(resp) = persist_document(&config_path, doc_text) {
            return resp;
        }

    let cluster = config.cluster;
    // Update in-memory cluster config so the UI backfills on refresh without requiring restart.
    if let Some(app_state) = state.app_state() {
        app_state.update_cluster_config(cluster.clone());
    }
    Json(json!({
        "status": "ok",
        "requires_restart": true,
        "message": "Cluster settings saved. Please restart the service for changes to take effect.",
        "cluster": cluster,
    }))
    .into_response()
}

/// List registered export/reload addon handlers (no feature gate needed).
#[cfg(feature = "commerce")]
async fn list_reload_addons_handler(
    State(state): State<Arc<ConsoleState>>,
) -> Response {
    let items: Vec<serde_json::Value> = if let Some(app_state) = state.app_state() {
        app_state.addon_registry.export_reload.list()
            .into_iter()
            .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
            .collect()
    } else {
        Vec::new()
    };
    Json(json!({ "addons": items })).into_response()
}

/// SSE-based reload that processes current node + all peers.
#[cfg(feature = "commerce")]
async fn cluster_reload_sse_handler(
    State(state): State<Arc<ConsoleState>>,
    Query(query): Query<crate::handler::ami::PingReloadPayload>,
) -> Response {
    use axum::response::sse::Event as SseEvent;

    let app_state = match state.app_state() {
        Some(s) => s,
        None => {
            use axum::response::sse::Sse;
            return Sse::new(futures::stream::once(async move {
                Ok::<_, std::convert::Infallible>(SseEvent::default().event("error").data(r#"{"error":"PBX not running"}"#))
            }))
            .into_response();
        }
    };

    use axum::response::sse::{KeepAlive, Sse};
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<SseEvent, std::convert::Infallible>>();

    let payload = crate::handler::ami::PingReloadPayload {
        trunks: query.trunks,
        routes: query.routes,
        addons: query.addons,
    };

    let app = app_state.clone();
    tokio::spawn(async move {
        macro_rules! sse_send {
            ($event_type:expr, $data:expr) => {
                let tx = tx.clone();
                let _ = tx.send(Ok(SseEvent::default().event($event_type).data($data.to_string())));
            };
        }

        // Process trunks on current node
        if payload.trunks {
            sse_send!("progress", serde_json::json!({"type": "addon_start", "node": "current", "addon": "trunks"}));
            let result = reload_trunks(&app, "current").await;
            sse_send!("progress", serde_json::json!({"type": "addon_complete", "node": "current", "addon": "trunks", "result": result}));
        }

        // Process routes on current node
        if payload.routes {
            sse_send!("progress", serde_json::json!({"type": "addon_start", "node": "current", "addon": "routes"}));
            let result = reload_routes_console(&app, "current").await;
            sse_send!("progress", serde_json::json!({"type": "addon_complete", "node": "current", "addon": "routes", "result": result}));
        }

        // Process addon-based handlers on current node
        for addon_id in &payload.addons {
            sse_send!("progress", serde_json::json!({"type": "addon_start", "node": "current", "addon": addon_id}));
            let results = app.addon_registry.export_reload
                .invoke_selected(&[addon_id.clone()], &app)
                .await;
            let json_result = match results.into_iter().next() {
                Some((_, Ok(v))) => serde_json::json!({ "status": "ok", "details": v }),
                Some((_, Err(e))) => serde_json::json!({ "status": "error", "message": e }),
                None => serde_json::json!({ "status": "error", "message": "Handler not found" }),
            };
            sse_send!("progress", serde_json::json!({"type": "addon_complete", "node": "current", "addon": addon_id, "result": json_result}));
        }

        // Process peer nodes
        let peers = app.config().cluster.as_ref()
            .map(|c| c.peers.clone())
            .unwrap_or_default();

        let ami_path = app
            .config()
            .proxy
            .ami_path
            .clone()
            .unwrap_or_else(|| "/ami/v1".to_string());

        for peer in &peers {
            let peer_label = format!("{}:{}", peer.addr, peer.ami_port);
            sse_send!("progress", serde_json::json!({"type": "node_start", "node": &peer_label}));

            let url = format!("http://{}:{}{}/cluster/reload_sync", peer.addr, peer.ami_port, ami_path);
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default();

            match client.post(&url).json(&payload).send().await {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(peer_results) => {
                        sse_send!("progress", serde_json::json!({"type": "node_complete", "node": &peer_label, "result": peer_results}));
                    }
                    Err(e) => {
                        sse_send!("progress", serde_json::json!({"type": "node_error", "node": &peer_label, "error": format!("Invalid JSON: {}", e)}));
                    }
                },
                Err(e) => {
                    sse_send!("progress", serde_json::json!({"type": "node_error", "node": &peer_label, "error": format!("Connection failed: {}", e)}));
                }
            }
        }

        sse_send!("complete", serde_json::json!({"type": "complete"}));
    });

    let sse_stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    });
    Sse::new(sse_stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)).text("keep-alive"))
        .into_response()
}

/// Reload trunks helper (mirrors the AMI handler's logic).
#[cfg(feature = "commerce")]
async fn reload_trunks(app: &crate::app::AppStateInner, _node: &str) -> serde_json::Value {
    let config_path = app.config_path.clone();
    let config_override = config_path.as_ref().and_then(|path| {
        crate::config::Config::load(path).ok().map(|cfg| std::sync::Arc::new(cfg.proxy))
    });

    match app
        .sip_server()
        .inner
        .data_context
        .reload_trunks(true, config_override)
        .await
    {
        Ok(metrics) => {
            if let Some(ref console) = app.console {
                console.clear_pending_reload();
            }
            serde_json::json!({ "addon": "trunks", "status": "ok", "reloaded": metrics.total })
        }
        Err(e) => serde_json::json!({ "addon": "trunks", "status": "error", "message": e.to_string() }),
    }
}

/// Reload routes helper (mirrors the AMI handler's logic).
#[cfg(feature = "commerce")]
async fn reload_routes_console(app: &crate::app::AppStateInner, _node: &str) -> serde_json::Value {
    let config_path = app.config_path.clone();
    let config_override = config_path.as_ref().and_then(|path| {
        crate::config::Config::load(path).ok().map(|cfg| std::sync::Arc::new(cfg.proxy))
    });

    match app
        .sip_server()
        .inner
        .data_context
        .reload_routes(true, config_override)
        .await
    {
        Ok(metrics) => {
            if let Some(ref console) = app.console {
                console.clear_pending_reload();
            }
            serde_json::json!({ "addon": "routes", "status": "ok", "reloaded": metrics.total })
        }
        Err(e) => serde_json::json!({ "addon": "routes", "status": "error", "message": e.to_string() }),
    }
}

const LOG_DEFAULT_LIMIT: usize = 200;
const LOG_MAX_LIMIT: usize = 2000;

struct FollowReadResult {
    lines: Vec<String>,
    next_position: u64,
    reset: bool,
    truncated: bool,
}

async fn fetch_recent_logs(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Query(query): Query<LogRecentQuery>,
) -> Response {
    if !user.is_superuser {
        return json_error(StatusCode::FORBIDDEN, "Permission denied. Superuser required.");
    }

    let Some(path) = resolve_log_file_path(&state) else {
        return Json(json!({
            "status": "ok",
            "available": false,
            "exists": false,
            "path": JsonValue::Null,
            "lines": [],
            "next_position": 0u64,
            "reset": false,
            "truncated": false,
            "message": "Log file is not configured. Set settings -> platform -> log_file first.",
        }))
        .into_response();
    };

    let limit = normalize_log_limit(query.limit);
    match read_recent_log_lines(&path, limit) {
        Ok((lines, next_position, truncated)) => Json(json!({
            "status": "ok",
            "available": true,
            "exists": true,
            "path": path,
            "lines": lines,
            "next_position": next_position,
            "reset": false,
            "truncated": truncated,
            "message": JsonValue::Null,
        }))
        .into_response(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Json(json!({
            "status": "ok",
            "available": true,
            "exists": false,
            "path": path,
            "lines": [],
            "next_position": 0u64,
            "reset": false,
            "truncated": false,
            "message": "Log file does not exist yet.",
        }))
        .into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to read log file: {err}"),
        ),
    }
}

async fn follow_logs(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Query(query): Query<LogFollowQuery>,
) -> Response {
    if !user.is_superuser {
        return json_error(StatusCode::FORBIDDEN, "Permission denied. Superuser required.");
    }

    let Some(path) = resolve_log_file_path(&state) else {
        return Json(json!({
            "status": "ok",
            "available": false,
            "exists": false,
            "path": JsonValue::Null,
            "lines": [],
            "next_position": 0u64,
            "reset": false,
            "truncated": false,
            "message": "Log file is not configured. Set settings -> platform -> log_file first.",
        }))
        .into_response();
    };

    let position = query.position.unwrap_or(0);
    let limit = normalize_log_limit(query.limit);

    match read_follow_log_lines(&path, position, limit) {
        Ok(result) => Json(json!({
            "status": "ok",
            "available": true,
            "exists": true,
            "path": path,
            "lines": result.lines,
            "next_position": result.next_position,
            "reset": result.reset,
            "truncated": result.truncated,
            "message": JsonValue::Null,
        }))
        .into_response(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Json(json!({
            "status": "ok",
            "available": true,
            "exists": false,
            "path": path,
            "lines": [],
            "next_position": 0u64,
            "reset": position > 0,
            "truncated": false,
            "message": "Log file does not exist yet.",
        }))
        .into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to follow log file: {err}"),
        ),
    }
}

async fn stream_logs(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Query(query): Query<LogStreamQuery>,
) -> Response {
    if !user.is_superuser {
        return json_error(StatusCode::FORBIDDEN, "Permission denied. Superuser required.");
    }

    let Some(path) = resolve_log_file_path(&state) else {
        return Json(json!({
            "status": "ok",
            "available": false,
            "exists": false,
            "path": JsonValue::Null,
            "lines": [],
            "next_position": 0u64,
            "reset": false,
            "truncated": false,
            "message": "Log file is not configured. Set settings -> platform -> log_file first.",
        }))
        .into_response();
    };

    let start_position = query.position.unwrap_or(0);
    let limit = normalize_log_limit(query.limit);
    let path_for_stream = path.clone();

    let stream = stream::unfold(start_position, move |mut cursor| {
        let path = path_for_stream.clone();
        async move {
            time::sleep(StdDuration::from_millis(1000)).await;

            let payload = match read_follow_log_lines(&path, cursor, limit) {
                Ok(result) => {
                    cursor = result.next_position;
                    json!({
                        "status": "ok",
                        "path": path,
                        "lines": result.lines,
                        "next_position": result.next_position,
                        "reset": result.reset,
                        "truncated": result.truncated,
                    })
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    cursor = 0;
                    json!({
                        "status": "ok",
                        "path": path,
                        "lines": [],
                        "next_position": 0u64,
                        "reset": true,
                        "truncated": false,
                        "exists": false,
                        "message": "Log file does not exist yet.",
                    })
                }
                Err(err) => json!({
                    "status": "error",
                    "message": format!("Failed to follow log file: {err}"),
                }),
            };

            let event = axum::response::sse::Event::default().event("logs").data(payload.to_string());
            Some((Ok::<_, Infallible>(event), cursor))
        }
    });

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(StdDuration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

fn normalize_log_limit(limit: Option<usize>) -> usize {
    match limit {
        Some(value) => value.clamp(1, LOG_MAX_LIMIT),
        None => LOG_DEFAULT_LIMIT,
    }
}

fn resolve_log_file_path(state: &ConsoleState) -> Option<String> {
    state.app_state().and_then(|app| {
        app.config()
            .log_file
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

fn read_recent_log_lines(path: &str, limit: usize) -> io::Result<(Vec<String>, u64, bool)> {
    let file = File::open(path)?;
    let next_position = file.metadata()?.len();
    let reader = BufReader::new(file);
    let mut lines = VecDeque::new();
    let mut truncated = false;

    for line in reader.lines() {
        let line = line?;
        if lines.len() == limit {
            lines.pop_front();
            truncated = true;
        }
        lines.push_back(line);
    }

    Ok((lines.into_iter().collect(), next_position, truncated))
}

fn read_follow_log_lines(path: &str, position: u64, limit: usize) -> io::Result<FollowReadResult> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();

    if position > file_len {
        let (lines, next_position, truncated) = read_recent_log_lines(path, limit)?;
        return Ok(FollowReadResult {
            lines,
            next_position,
            reset: true,
            truncated,
        });
    }

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(position))?;

    let mut lines = Vec::new();
    let mut raw = String::new();
    let mut truncated = false;

    while lines.len() < limit {
        raw.clear();
        let read = reader.read_line(&mut raw)?;
        if read == 0 {
            break;
        }
        lines.push(raw.trim_end_matches(&['\n', '\r'][..]).to_string());
    }

    if lines.len() == limit {
        let mut extra = String::new();
        let extra_read = reader.read_line(&mut extra)?;
        if extra_read > 0 {
            truncated = true;
            let rewind = i64::try_from(extra_read).unwrap_or(i64::MAX);
            reader.seek(SeekFrom::Current(-rewind))?;
        }
    }

    let next_position = reader.stream_position()?;

    Ok(FollowReadResult {
        lines,
        next_position,
        reset: false,
        truncated,
    })
}

#[allow(clippy::result_large_err)]
fn get_config_path(state: &ConsoleState) -> Result<String, Response> {
    let Some(app_state) = state.app_state() else {
        return Err(json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Application state is unavailable.",
        ));
    };

    let Some(path) = app_state.config_path.clone() else {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "Configuration file path is unknown. Start the service with --conf to enable editing.",
        ));
    };
    Ok(path)
}

#[allow(clippy::result_large_err)]
fn load_document(path: &str) -> Result<DocumentMut, Response> {
    let contents = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            return Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read configuration file: {}", err),
            ));
        }
    };

    contents.parse::<DocumentMut>().map_err(|err| {
        json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("Configuration file is not valid TOML: {}", err),
        )
    })
}

#[allow(dead_code)]
#[allow(clippy::result_large_err)]
fn persist_document(path: &str, contents: String) -> Result<(), Response> {
    fs::write(path, contents).map_err(|err| {
        json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to write configuration file: {}", err),
        )
    })
}

#[allow(clippy::result_large_err)]
fn parse_config_from_str(contents: &str) -> Result<Config, Response> {
    toml::from_str::<Config>(contents)
        .map(|mut cfg| {
            cfg.ensure_recording_defaults();
            cfg
        })
        .map_err(|err| {
            json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Configuration validation failed: {}", err),
            )
        })
}

fn ensure_table_mut<'doc>(doc: &'doc mut DocumentMut, key: &str) -> &'doc mut Table {
    let needs_init = doc
        .as_table()
        .get(key)
        .map(|item| !item.is_table())
        .unwrap_or(true);

    if needs_init {
        doc.insert(key, Item::Table(Table::new()));
    }

    doc.as_table_mut()
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .expect("table")
}

fn parse_lines_to_vec(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

fn set_string_array(table: &mut Table, key: &str, values: Vec<String>) {
    let mut array = Array::new();
    for value in values {
        array.push(value.as_str());
    }
    table[key] = Item::Value(Value::Array(array));
}

pub(crate) async fn test_storage_connection(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<TestStoragePayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    use crate::storage::{Storage, StorageConfig};
    use uuid::Uuid;

    let config = StorageConfig::S3 {
        vendor: payload.vendor,
        bucket: payload.bucket,
        region: payload.region,
        access_key: payload.access_key,
        secret_key: payload.secret_key,
        endpoint: payload.endpoint,
        prefix: payload.root,
    };

    let storage = match Storage::new(&config) {
        Ok(s) => s,
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                format!("Failed to initialize storage: {}", err),
            );
        }
    };

    let filename = format!("test-connection-{}.txt", Uuid::new_v4());
    let content = b"RustPBX storage connection test";

    let test_fut = async {
        storage
            .write(&filename, bytes::Bytes::from_static(content))
            .await?;
        if let Err(err) = storage.delete(&filename).await {
            warn!("Failed to delete test file {}: {}", filename, err);
        }
        Ok::<_, anyhow::Error>(())
    };

    match tokio::time::timeout(std::time::Duration::from_secs(10), test_fut).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                format!("Failed to write test file: {}", err),
            );
        }
        Err(_) => {
            return json_error(
                StatusCode::REQUEST_TIMEOUT,
                "Storage connection timed out after 10 seconds",
            );
        }
    }

    Json(json!({
        "status": "ok",
        "message": "Connection successful. Test file created and deleted.",
    }))
    .into_response()
}

pub(crate) async fn test_locator_webhook(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<TestLocatorWebhookPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut request = client.post(&payload.url);
    if let Some(headers) = payload.headers {
        for (k, v) in headers {
            request = request.header(k, v);
        }
    }

    let test_event = json!({
        "event": "test",
        "timestamp": Utc::now().timestamp(),
        "message": "RustPBX locator webhook test"
    });

    match request.json(&test_event).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                Json(json!({
                    "status": "ok",
                    "message": format!("Webhook test successful: HTTP {}", resp.status()),
                }))
                .into_response()
            } else {
                json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Webhook returned error: HTTP {}", resp.status()),
                )
            }
        }
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            format!("Webhook request failed: {}", err),
        ),
    }
}

pub(crate) async fn test_http_router(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<TestHttpRouterPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut request = client.post(&payload.url);
    if let Some(headers) = payload.headers {
        for (k, v) in headers {
            request = request.header(k, v);
        }
    }

    let test_request = json!({
        "call_id": "test-call-id",
        "from": "sip:test@localhost",
        "to": "sip:echo@localhost",
        "method": "INVITE",
        "uri": "sip:echo@localhost",
        "direction": "internal"
    });

    match request.json(&test_request).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                Json(json!({
                    "status": "ok",
                    "message": format!("HTTP Router test successful: HTTP {}", resp.status()),
                }))
                .into_response()
            } else {
                json_error(
                    StatusCode::BAD_REQUEST,
                    format!("HTTP Router returned error: HTTP {}", resp.status()),
                )
            }
        }
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            format!("HTTP Router request failed: {}", err),
        ),
    }
}

pub(crate) async fn test_user_backend(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<TestUserBackendPayload>,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    match payload.backend {
        UserBackendConfig::Memory { .. } => Json(json!({
            "status": "ok",
            "message": "Memory backend configuration is valid."
        }))
        .into_response(),
        UserBackendConfig::Http { url, .. } => {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());

            match client.get(&url).send().await {
                Ok(resp) => Json(json!({
                    "status": "ok",
                    "message": format!("HTTP backend reachable: HTTP {}", resp.status()),
                }))
                .into_response(),
                Err(err) => json_error(
                    StatusCode::BAD_REQUEST,
                    format!("HTTP backend unreachable: {}", err),
                ),
            }
        }
        UserBackendConfig::Database { url, .. } => {
            if let Some(db_url) = url {
                Json(json!({
                    "status": "ok",
                    "message": format!("Database URL configured: {}", db_url)
                }))
                .into_response()
            } else {
                json_error(StatusCode::BAD_REQUEST, "Database URL is missing")
            }
        }
        UserBackendConfig::Plain { path } => {
            if std::path::Path::new(&path).exists() {
                Json(json!({
                    "status": "ok",
                    "message": format!("Plain file exists: {}", path)
                }))
                .into_response()
            } else {
                json_error(
                    StatusCode::BAD_REQUEST,
                    format!("Plain file does not exist: {}", path),
                )
            }
        }
        UserBackendConfig::Extension { .. } => Json(json!({
            "status": "ok",
            "message": "Extension backend uses internal database."
        }))
        .into_response(),
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "status": "error",
            "message": message.into(),
        })),
    )
        .into_response()
}

async fn list_roles(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    match RoleEntity::find()
        .order_by_asc(RoleColumn::Name)
        .all(state.db())
        .await
    {
        Ok(roles) => Json(json!({ "items": roles })).into_response(),
        Err(err) => {
            warn!("failed to list roles: {}", err);
            json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

async fn get_role(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = state.db();
    let role = match RoleEntity::find_by_id(id).one(db).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "Role not found"),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    let perms = match role_permission::Entity::find()
        .filter(role_permission::Column::RoleId.eq(id))
        .all(db)
        .await
    {
        Ok(p) => p,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    Json(json!({ "role": role, "permissions": perms })).into_response()
}

async fn create_role(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<RolePayload>,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let name = payload.name.trim();
    if name.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "Role name is required");
    }
    let now = Utc::now();
    let active = RoleActiveModel {
        name: Set(name.to_string()),
        description: Set(Some(payload.description.unwrap_or_default())),
        is_system: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let inserted = match RoleEntity::insert(active)
        .exec_with_returning(state.db())
        .await
    {
        Ok(r) => r,
        Err(err) => {
            warn!("failed to create role: {}", err);
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
        }
    };
    for entry in &payload.permissions {
        let perm = role_permission::ActiveModel {
            role_id: Set(inserted.id),
            resource: Set(entry.resource.clone()),
            action: Set(entry.action.clone()),
            ..Default::default()
        };
        if let Err(err) = role_permission::Entity::insert(perm).exec(state.db()).await {
            warn!("failed to insert permission: {}", err);
        }
    }
    (
        StatusCode::CREATED,
        Json(json!({ "status": "ok", "id": inserted.id })),
    )
        .into_response()
}

async fn update_role(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<RolePayload>,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = state.db();
    let model = match RoleEntity::find_by_id(id).one(db).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "Role not found"),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    if model.is_system {
        return json_error(StatusCode::FORBIDDEN, "Cannot modify system roles");
    }
    let mut active: RoleActiveModel = model.into();
    let name = payload.name.trim();
    if !name.is_empty() {
        active.name = Set(name.to_string());
    }
    if let Some(desc) = payload.description {
        active.description = Set(Some(desc));
    }
    active.updated_at = Set(Utc::now());
    if let Err(err) = active.update(db).await {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }
    if let Err(err) = role_permission::Entity::delete_many()
        .filter(role_permission::Column::RoleId.eq(id))
        .exec(db)
        .await
    {
        warn!("failed to clear permissions for role {}: {}", id, err);
    }
    for entry in &payload.permissions {
        let perm = role_permission::ActiveModel {
            role_id: Set(id),
            resource: Set(entry.resource.clone()),
            action: Set(entry.action.clone()),
            ..Default::default()
        };
        if let Err(err) = role_permission::Entity::insert(perm).exec(db).await {
            warn!("failed to insert permission: {}", err);
        }
    }
    Json(json!({ "status": "ok" })).into_response()
}

async fn delete_role_handler(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = state.db();
    let model = match RoleEntity::find_by_id(id).one(db).await {
        Ok(Some(r)) => r,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "Role not found"),
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    if model.is_system {
        return json_error(StatusCode::FORBIDDEN, "Cannot delete system roles");
    }
    match RoleEntity::delete_by_id(id).exec(db).await {
        Ok(_) => Json(json!({ "status": "ok" })).into_response(),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

async fn get_user_roles(
    AxumPath(user_id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = state.db();
    let assignments = match user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_id))
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    let role_ids: Vec<i64> = assignments.iter().map(|r| r.role_id).collect();
    let roles = match RoleEntity::find()
        .filter(RoleColumn::Id.is_in(role_ids))
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };
    Json(json!({ "items": roles })).into_response()
}

async fn assign_user_roles(
    AxumPath(user_id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<AssignRolesPayload>,
) -> Response {
    if !state.has_permission(&user, "users", "manage").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = state.db();
    if let Err(err) = user_role::Entity::delete_many()
        .filter(user_role::Column::UserId.eq(user_id))
        .exec(db)
        .await
    {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }
    let now = Utc::now();
    for role_id in &payload.role_ids {
        let assignment = user_role::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(*role_id),
            created_at: Set(now),
            ..Default::default()
        };
        if let Err(err) = user_role::Entity::insert(assignment).exec(db).await {
            warn!(
                "failed to assign role {} to user {}: {}",
                role_id, user_id, err
            );
        }
    }
    Json(json!({ "status": "ok" })).into_response()
}

// ─── Pending S3 uploads (failed-upload retry queue) ─────────────────────────

/// GET /settings/uploads/pending
///
/// Returns the recent pending_uploads rows plus counts grouped by status.
/// Used by the "Pending S3 uploads" panel on the Recording tab.
pub(crate) async fn list_pending_uploads(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "system", "read").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };

    let counts =
        match crate::models::pending_upload::Model::count_by_status(&db).await {
            Ok(c) => c,
            Err(e) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to count pending uploads: {e}"),
                );
            }
        };

    let rows = match crate::models::pending_upload::Model::list_recent(&db, 50).await {
        Ok(r) => r,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list pending uploads: {e}"),
            );
        }
    };

    let entries: Vec<JsonValue> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "call_id": r.call_id,
                "kind": r.kind,
                "local_path": r.local_path,
                "target_key": r.target_key,
                "attempts": r.attempts,
                "status": r.status,
                "last_error": r.last_error,
                "last_attempt_at": r.last_attempt_at.map(|t| t.to_rfc3339()),
                "created_at": r.created_at.to_rfc3339(),
            })
        })
        .collect();

    Json(json!({
        "status": "ok",
        "counts": {
            "pending": counts.0,
            "failed_permanent": counts.1,
            "failed_missing_source": counts.2,
        },
        "entries": entries,
    }))
    .into_response()
}

/// POST /settings/uploads/pending/retry
///
/// Resets every non-pending row back to `pending` (so failed_permanent
/// rows get a fresh chance) and triggers an immediate sweep of the
/// upload-retry scheduler. Returns the post-sweep counts.
pub(crate) async fn retry_pending_uploads(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };
    let reset = match crate::models::pending_upload::Model::reset_failed(&db).await {
        Ok(n) => n,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to reset failed rows: {e}"),
            );
        }
    };

    let app_state = match state.app_state() {
        Some(a) => a,
        None => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Application state unavailable",
            );
        }
    };
    if let Err(e) = crate::upload_retry::sweep(&app_state, 10).await {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Sweep failed: {e}"),
        );
    }

    let counts = match crate::models::pending_upload::Model::count_by_status(&db).await {
        Ok(c) => c,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to count after sweep: {e}"),
            );
        }
    };
    Json(json!({
        "status": "ok",
        "reset_to_pending": reset,
        "counts": {
            "pending": counts.0,
            "failed_permanent": counts.1,
            "failed_missing_source": counts.2,
        },
    }))
    .into_response()
}

/// POST /settings/uploads/pending/clear
///
/// Deletes every row in `failed_permanent` or `failed_missing_source`.
/// Used by the "Clear failures" button in the panel — these are rows
/// the operator has acknowledged and decided not to retry.
pub(crate) async fn clear_pending_failures(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "system", "write").await {
        return json_error(StatusCode::FORBIDDEN, "Permission denied");
    }
    let db = match get_db(&state) {
        Ok(db) => db,
        Err(r) => return r,
    };
    let cleared = match crate::models::pending_upload::Model::clear_failures(&db).await {
        Ok(n) => n,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to clear failures: {e}"),
            );
        }
    };
    Json(json!({
        "status": "ok",
        "cleared": cleared,
    }))
    .into_response()
}

// ── API key management (superuser / system:write only) ───────────────────────

#[derive(Debug, Deserialize)]
struct CreateApiKeyPayload {
    pub name: String,
    pub description: Option<String>,
}

fn is_authorized_for_api_keys(state: &ConsoleState, user: &UserModel) -> bool {
    user.is_superuser
}

async fn list_api_keys(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !is_authorized_for_api_keys(&state, &user) {
        return (StatusCode::FORBIDDEN, Json(json!({ "message": "Forbidden" }))).into_response();
    }

    let db = state.db();

    match ApiKeyEntity::find()
        .order_by_desc(ApiKeyColumn::CreatedAt)
        .all(db)
        .await
    {
        Ok(keys) => {
            let items: Vec<JsonValue> = keys
                .into_iter()
                .map(|k| {
                    json!({
                        "id": k.id,
                        "name": k.name,
                        "description": k.description,
                        "created_at": k.created_at.to_rfc3339(),
                        "last_used_at": k.last_used_at.map(|t| t.to_rfc3339()),
                        "revoked_at": k.revoked_at.map(|t| t.to_rfc3339()),
                        "is_active": k.revoked_at.is_none(),
                    })
                })
                .collect();
            Json(json!({ "items": items })).into_response()
        }
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list API keys: {e}"),
        ),
    }
}

async fn create_api_key(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Json(payload): Json<CreateApiKeyPayload>,
) -> Response {
    if !is_authorized_for_api_keys(&state, &user) {
        return (StatusCode::FORBIDDEN, Json(json!({ "message": "Forbidden" }))).into_response();
    }

    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "message": "name is required" })),
        )
            .into_response();
    }

    let db = state.db();
    let issued = issue_api_key();
    let plaintext = issued.plaintext.clone();
    let am = ApiKeyActiveModel {
        name: Set(name.clone()),
        hash_sha256: Set(issued.hash),
        description: Set(payload.description.and_then(|d| {
            let t = d.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        })),
        created_at: Set(Utc::now()),
        ..Default::default()
    };

    match am.insert(db).await {
        Ok(key) => Json(json!({
            "id": key.id,
            "name": key.name,
            "description": key.description,
            "created_at": key.created_at.to_rfc3339(),
            "plaintext": plaintext,
            "is_active": true,
        }))
        .into_response(),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("UNIQUE") || msg.contains("unique") {
                (
                    StatusCode::CONFLICT,
                    Json(json!({ "message": "An API key with this name already exists" })),
                )
                    .into_response()
            } else {
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to create API key: {e}"),
                )
            }
        }
    }
}

async fn revoke_api_key(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    AxumPath(id): AxumPath<i64>,
) -> Response {
    if !is_authorized_for_api_keys(&state, &user) {
        return (StatusCode::FORBIDDEN, Json(json!({ "message": "Forbidden" }))).into_response();
    }

    let db = state.db();

    let key = match ApiKeyEntity::find_by_id(id).one(db).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "message": "API key not found" })),
            )
                .into_response();
        }
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to fetch API key: {e}"),
            );
        }
    };

    if key.revoked_at.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "message": "API key is already revoked" })),
        )
            .into_response();
    }

    let mut am: ApiKeyActiveModel = key.into();
    am.revoked_at = Set(Some(Utc::now()));
    match am.update(db).await {
        Ok(updated) => Json(json!({
            "id": updated.id,
            "name": updated.name,
            "revoked_at": updated.revoked_at.map(|t| t.to_rfc3339()),
            "is_active": false,
        }))
        .into_response(),
        Err(e) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to revoke API key: {e}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConsoleConfig;
    use crate::models::migration::Migrator;
    use crate::models::rbac;
    use sea_orm::Database;
    use sea_orm_migration::MigratorTrait;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn superuser() -> UserModel {
        let now = Utc::now();
        UserModel {
            id: 1,
            email: "admin@test.com".into(),
            username: "admin".into(),
            password_hash: "x".into(),
            reset_token: None,
            reset_token_expires: None,
            last_login_at: None,
            last_login_ip: None,
            created_at: now,
            updated_at: now,
            is_active: true,
            is_staff: true,
            is_superuser: true,
            mfa_enabled: false,
            mfa_secret: None,
            auth_source: "local".into(),
        }
    }

    async fn setup_state() -> Arc<ConsoleState> {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite memory");
        Migrator::up(&db, None).await.expect("run migrations");
        ConsoleState::initialize(
            Arc::new(crate::callrecord::DefaultCallRecordFormatter::default()),
            db,
            ConsoleConfig::default(),
        )
        .await
        .expect("initialize console state")
    }

    #[tokio::test]
    async fn list_roles_returns_seeded_roles() {
        let state = setup_state().await;
        let user = superuser();
        let response = list_roles(State(state), AuthRequired(user)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
        let items = parsed["items"].as_array().expect("items");
        assert_eq!(items.len(), rbac::SYSTEM_ROLES.len());
    }

    #[tokio::test]
    async fn create_and_delete_custom_role() {
        let state = setup_state().await;
        let user = superuser();

        let payload = RolePayload {
            name: "test_custom".into(),
            description: Some("Test role".into()),
            permissions: vec![PermissionEntry {
                resource: "extensions".into(),
                action: "read".into(),
            }],
        };
        let create_resp = create_role(
            State(state.clone()),
            AuthRequired(user.clone()),
            Json(payload),
        )
        .await;
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(create_resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
        let role_id = parsed["id"].as_i64().expect("role id");

        let del_resp =
            delete_role_handler(AxumPath(role_id), State(state.clone()), AuthRequired(user)).await;
        assert_eq!(del_resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cannot_delete_system_role() {
        let state = setup_state().await;
        let user = superuser();
        let roles = rbac::Entity::find()
            .all(state.db())
            .await
            .expect("query roles");
        let system_role = roles.first().expect("at least one role");
        let resp =
            delete_role_handler(AxumPath(system_role.id), State(state), AuthRequired(user)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn assign_and_fetch_user_roles() {
        let state = setup_state().await;
        let user = superuser();
        let roles = rbac::Entity::find()
            .all(state.db())
            .await
            .expect("query roles");
        let viewer = roles.iter().find(|r| r.name == "viewer").expect("viewer");

        let assign_resp = assign_user_roles(
            AxumPath(42i64),
            State(state.clone()),
            AuthRequired(user.clone()),
            Json(AssignRolesPayload {
                role_ids: vec![viewer.id],
            }),
        )
        .await;
        assert_eq!(assign_resp.status(), StatusCode::OK);

        let fetch_resp = get_user_roles(AxumPath(42i64), State(state), AuthRequired(user)).await;
        assert_eq!(fetch_resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(fetch_resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
        let items = parsed["items"].as_array().expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], "viewer");
    }

    #[test]
    fn read_recent_log_lines_limits_tail() {
        let mut file = NamedTempFile::new().expect("tempfile");
        writeln!(file, "line-1").expect("write line 1");
        writeln!(file, "line-2").expect("write line 2");
        writeln!(file, "line-3").expect("write line 3");

        let path = file.path().to_string_lossy().to_string();
        let (lines, next_position, truncated) =
            read_recent_log_lines(&path, 2).expect("read recent logs");

        assert_eq!(lines, vec!["line-2".to_string(), "line-3".to_string()]);
        assert!(next_position > 0);
        assert!(truncated);
    }

    #[test]
    fn follow_logs_resets_on_rotation() {
        let mut file = NamedTempFile::new().expect("tempfile");
        writeln!(file, "new-1").expect("write new-1");
        writeln!(file, "new-2").expect("write new-2");

        let path = file.path().to_string_lossy().to_string();
        let result = read_follow_log_lines(&path, 10_000, 200).expect("follow logs");

        assert!(result.reset);
        assert_eq!(result.lines, vec!["new-1".to_string(), "new-2".to_string()]);
        assert!(result.next_position > 0);
    }

    #[test]
    fn follow_logs_keeps_position_when_truncated() {
        let mut file = NamedTempFile::new().expect("tempfile");
        writeln!(file, "l1").expect("write l1");
        writeln!(file, "l2").expect("write l2");
        writeln!(file, "l3").expect("write l3");

        let path = file.path().to_string_lossy().to_string();
        let first = read_follow_log_lines(&path, 0, 2).expect("first follow");
        assert_eq!(first.lines, vec!["l1".to_string(), "l2".to_string()]);
        assert!(first.truncated);

        let second = read_follow_log_lines(&path, first.next_position, 2).expect("second follow");
        assert_eq!(second.lines, vec!["l3".to_string()]);
        assert!(!second.reset);
    }
}
