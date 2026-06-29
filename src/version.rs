use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::debug;

#[allow(unused_imports)]
use tracing::info;

const VERSION_INFO: &str = concat!(
    "rustpbx ",
    env!("CARGO_PKG_VERSION"),
    "\nBuild Time: ",
    env!("BUILD_TIME_FMT"),
    "\nGit Commit: ",
    env!("GIT_COMMIT_HASH"),
    "\nGit Branch: ",
    env!("GIT_BRANCH"),
    "\nGit Status: ",
    env!("GIT_DIRTY")
);

const SHORT_VERSION: &str = env!("SHORT_VERSION");

pub fn get_version_info() -> &'static str {
    VERSION_INFO
}

pub fn get_short_version() -> &'static str {
    SHORT_VERSION
}

/// Default brand token when `SUPERSBC_USER_AGENT` is unset.
const DEFAULT_BRAND: &str = "SuperSBC";

/// Env var holding the brand *token* (e.g. `SuperSBC`); the `/{version} (built
/// {date})` suffix is added by [`get_useragent`], so the token alone is enough.
const BRAND_ENV: &str = "SUPERSBC_USER_AGENT";

/// Resolve a brand token from a raw env value. Pure (no global state) so it is
/// trivially testable; blank/whitespace-only falls back to [`DEFAULT_BRAND`].
fn brand_from(raw: Option<String>) -> String {
    raw.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BRAND.to_string())
}

/// Brand token for on-the-wire identity. Runtime-overridable via the
/// `SUPERSBC_USER_AGENT` env var.
pub fn brand() -> String {
    brand_from(std::env::var(BRAND_ENV).ok())
}

/// Format a full User-Agent for a given brand token. Pure core of
/// [`get_useragent`]; version and build date are compile-time.
fn useragent_with(brand: &str) -> String {
    format!(
        "{}/{} (built {})",
        brand,
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_DATE")
    )
}

/// Full User-Agent string, emitted identically across the SIP and webhook
/// surfaces: `{brand}/{version} (built {date})`.
pub fn get_useragent() -> String {
    useragent_with(&brand())
}

/// Normalize a brand token to a SIP-safe URI user-part (lowercased, whitespace
/// removed). Pure core of [`brand_sip_user`].
fn sip_user_from(brand: &str) -> String {
    brand.to_lowercase().split_whitespace().collect()
}

/// SIP-safe user-part for the Contact URI, derived from the brand token.
pub fn brand_sip_user() -> String {
    sip_user_from(&brand())
}

/// SIP-safe user-part derived from a literal `User-Agent` string (e.g.
/// `"AcmeSBC/1.0"` → `"acmesbc"`). Falls back to [`brand_sip_user`] when
/// `ua` is `None`. Used to keep the Contact user-part consistent with a
/// config-pinned `useragent` value.
pub fn brand_sip_user_from_ua(ua: Option<&str>) -> String {
    match ua {
        Some(s) => sip_user_from(s.split('/').next().unwrap_or(DEFAULT_BRAND)),
        None => brand_sip_user(),
    }
}

// ─── Update check ────────────────────────────────────────────────────────────

/// Response from the miuda.ai update-check endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub has_update: bool,
    pub latest_version: String,
    pub release_notes: Option<String>,
    pub download_url: Option<String>,
}

/// Query `https://miuda.ai/api/check_update` with current version + edition.
/// Returns `UpdateInfo` on success.
pub async fn check_update(start_time: Instant) -> anyhow::Result<UpdateInfo> {
    let version = env!("CARGO_PKG_VERSION");
    let edition = if cfg!(feature = "commerce") {
        "commerce"
    } else {
        "community"
    };
    let uptime_secs = start_time.elapsed().as_secs();
    let build_time = env!("BUILD_TIME_FMT");

    let client = reqwest::Client::new();
    let resp = client
        .get("https://miuda.ai/api/check_update")
        .query(&[
            ("version", version),
            ("edition", edition),
            ("uptime", &uptime_secs.to_string()),
            ("build_time", build_time),
        ])
        .header("User-Agent", get_useragent())
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) if e.is_timeout() || e.is_connect() => {
            anyhow::bail!("version check unreachable (network/timeout): {e}");
        }
        Err(e) => anyhow::bail!("version check request error: {e}"),
    };
    let status = resp.status();
    let body = resp.text().await?;
    debug!("version check response: status={} body={}", status, body);
    let info: UpdateInfo = serde_json::from_str(&body).map_err(|e| {
        anyhow::anyhow!("version check parse error: {e}, status={status}, body={body}")
    })?;
    Ok(info)
}

/// Spawn a background task that periodically checks for updates (at startup and
/// every 24 hours).  When a new version is found a `system_notification` row is
/// inserted into the database (deduped by title so the same version only appears
/// once).
pub fn spawn_update_checker(
    db: sea_orm::DatabaseConnection,
    token: tokio_util::sync::CancellationToken,
) {
    // Skip update check in debug/development mode
    #[cfg(debug_assertions)]
    {
        debug!("Skipping update check in debug mode");
        let _ = db;
        let _ = token;
    }

    #[cfg(not(debug_assertions))]
    tokio::spawn(async move {
        let start_time = Instant::now();
        loop {
            match check_update(start_time).await {
                Ok(info) if info.has_update => {
                    use crate::models::system_notification::{ActiveModel, Column, Entity};
                    use sea_orm::{
                        ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter,
                    };

                    let title = format!("New version available: {}", info.latest_version);
                    let exists = Entity::find()
                        .filter(Column::Title.eq(&title))
                        .one(&db)
                        .await
                        .ok()
                        .flatten()
                        .is_some();

                    if !exists {
                        let body = info.release_notes.clone().unwrap_or_default();
                        let am = ActiveModel {
                            id: sea_orm::ActiveValue::NotSet,
                            kind: Set("update".to_string()),
                            title: Set(title.clone()),
                            body: Set(body),
                            read: Set(false),
                            created_at: Set(chrono::Utc::now()),
                        };
                        match am.insert(&db).await {
                            Ok(_) => {
                                info!(latest = %info.latest_version, "update notification created")
                            }
                            Err(e) => debug!("failed to insert update notification: {e}"),
                        }
                    }
                }
                Ok(_) => debug!("version check: already up-to-date"),
                Err(e) => debug!("version check failed: {e}"),
            }

            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_defaults_when_unset() {
        assert_eq!(brand_from(None), DEFAULT_BRAND);
    }

    #[test]
    fn brand_honors_override_and_trims() {
        assert_eq!(brand_from(Some("supersbc".to_string())), "supersbc");
        assert_eq!(brand_from(Some("  AcmeSBC  ".to_string())), "AcmeSBC");
    }

    #[test]
    fn brand_blank_falls_back_to_default() {
        assert_eq!(brand_from(Some(String::new())), DEFAULT_BRAND);
        assert_eq!(brand_from(Some("   ".to_string())), DEFAULT_BRAND);
    }

    #[test]
    fn useragent_shape_for_default_brand() {
        let expected = format!(
            "SuperSBC/{} (built {})",
            env!("CARGO_PKG_VERSION"),
            env!("BUILD_DATE")
        );
        assert_eq!(useragent_with("SuperSBC"), expected);
    }

    #[test]
    fn useragent_reflects_override_brand() {
        let ua = useragent_with("AcmeSBC");
        assert!(ua.starts_with("AcmeSBC/"), "got: {ua}");
        assert!(ua.contains(env!("CARGO_PKG_VERSION")));
        assert!(ua.contains("(built "));
    }

    #[test]
    fn sip_user_is_lowercase_and_whitespace_free() {
        assert_eq!(sip_user_from("SuperSBC"), "supersbc");
        assert_eq!(sip_user_from("Super SBC"), "supersbc");
        assert_eq!(sip_user_from("  Acme  SBC "), "acmesbc");
    }

    #[test]
    fn brand_sip_user_from_ua_extracts_brand() {
        assert_eq!(brand_sip_user_from_ua(Some("AcmeSBC/1.0")), "acmesbc");
        assert_eq!(brand_sip_user_from_ua(Some("Super SBC/2.0 (built 2026)")), "supersbc");
        assert_eq!(brand_sip_user_from_ua(Some("NoSlash")), "noslash");
    }

    #[test]
    fn live_default_identity_is_consistent() {
        // Only meaningful when the override is absent (e.g. local/CI without the
        // var); guards against failing in environments that set a brand.
        if std::env::var(BRAND_ENV).is_err() {
            assert_eq!(brand(), DEFAULT_BRAND);
            assert_eq!(get_useragent(), useragent_with(DEFAULT_BRAND));
            assert_eq!(brand_sip_user(), "supersbc");
        }
    }
}
