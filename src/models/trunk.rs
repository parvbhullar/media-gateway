//! Unified `trunk` model — replaces the SIP-only `sip_trunk` model.
//!
//! See plan: `/home/anuj/.claude/plans/imperative-sauteeing-cake.md` (Phase 2).
//!
//! Storage: single `rustpbx_trunks` table with a `kind` discriminator column
//! (`"sip"`, `"webrtc"`, future kinds) and a `kind_config: Json` blob holding
//! all kind-specific configuration. Typed views (`SipTrunkConfig`,
//! `WebRtcTrunkConfig`) deserialize from `kind_config`.

use anyhow::{Result, anyhow, ensure};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Trunk operational status. Renamed from `SipTrunkStatus`; same variants.
#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[derive(Default)]
pub enum TrunkStatus {
    #[sea_orm(string_value = "healthy")]
    #[default]
    Healthy,
    #[sea_orm(string_value = "warning")]
    Warning,
    #[sea_orm(string_value = "standby")]
    Standby,
    #[sea_orm(string_value = "offline")]
    Offline,
}

impl TrunkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Standby => "standby",
            Self::Offline => "offline",
        }
    }
}

/// Trunk traffic direction. Renamed from `SipTrunkDirection`; same variants.
#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[derive(Default)]
pub enum TrunkDirection {
    #[sea_orm(string_value = "inbound")]
    Inbound,
    #[sea_orm(string_value = "outbound")]
    Outbound,
    #[sea_orm(string_value = "bidirectional")]
    #[default]
    Bidirectional,
}

impl TrunkDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
            Self::Bidirectional => "bidirectional",
        }
    }
}

/// SIP transport — still SIP-only, now lives inside `SipTrunkConfig` rather
/// than as a top-level column on the model.
#[derive(Copy, Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[derive(Default)]
pub enum SipTransport {
    #[sea_orm(string_value = "udp")]
    #[default]
    Udp,
    #[sea_orm(string_value = "tcp")]
    Tcp,
    #[sea_orm(string_value = "tls")]
    Tls,
}

impl SipTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
        }
    }
}

/// Canonical trunk row. Schema-shared, kind-agnostic, typed columns at top
/// level; kind-specific fields packed into `kind_config`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize, Default)]
#[sea_orm(table_name = "rustpbx_trunks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    #[sea_orm(unique)]
    pub name: String,
    pub kind: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub status: TrunkStatus,
    pub direction: TrunkDirection,
    pub is_active: bool,
    pub max_cps: Option<i32>,
    pub max_concurrent: Option<i32>,
    pub max_call_duration: Option<i32>,
    pub utilisation_percent: Option<f64>,
    pub warning_threshold_percent: Option<f64>,
    pub allowed_ips: Option<Json>,
    pub tags: Option<Json>,
    pub metadata: Option<Json>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub last_health_check_at: Option<DateTimeUtc>,
    pub health_check_interval_secs: Option<i32>,
    pub failure_threshold: Option<i32>,
    pub recovery_threshold: Option<i32>,
    #[sea_orm(default_value = "0")]
    pub consecutive_failures: i32,
    #[sea_orm(default_value = "0")]
    pub consecutive_successes: i32,
    pub kind_config: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Typed view of this row's `kind_config` as a `SipTrunkConfig`.
    /// Errors if `kind != "sip"` or the JSON does not match the schema.
    pub fn sip(&self) -> Result<SipTrunkConfig> {
        ensure!(
            self.kind == "sip",
            "kind mismatch: expected 'sip', got '{}'",
            self.kind
        );
        serde_json::from_value(self.kind_config.clone())
            .map_err(|e| anyhow!("failed to deserialize SipTrunkConfig from kind_config: {e}"))
    }

    /// Typed view of this row's `kind_config` as a `WebRtcTrunkConfig`.
    /// Errors if `kind != "webrtc"` or the JSON does not match the schema.
    pub fn webrtc(&self) -> Result<WebRtcTrunkConfig> {
        ensure!(
            self.kind == "webrtc",
            "kind mismatch: expected 'webrtc', got '{}'",
            self.kind
        );
        serde_json::from_value(self.kind_config.clone())
            .map_err(|e| anyhow!("failed to deserialize WebRtcTrunkConfig from kind_config: {e}"))
    }
}

/// Lightweight schema validation error type used by per-kind validators
/// (see Phase 3 — `src/proxy/bridge/kind_schemas.rs`).
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("{0}")]
    Custom(String),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl ValidationError {
    pub fn custom(s: impl Into<String>) -> Self {
        Self::Custom(s.into())
    }
}

/// Configuration shape for `kind = "sip"` trunks. All previously-typed
/// SIP-specific columns are packed here, serialized as JSON in the
/// `trunks.kind_config` column.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SipTrunkConfig {
    #[serde(default)]
    pub sip_server: Option<String>,
    #[serde(default)]
    pub sip_transport: SipTransport,
    #[serde(default)]
    pub outbound_proxy: Option<String>,
    #[serde(default)]
    pub auth_username: Option<String>,
    #[serde(default)]
    pub auth_password: Option<String>,
    #[serde(default)]
    pub register_enabled: bool,
    #[serde(default)]
    pub register_expires: Option<i32>,
    /// Matches the JSON shape used in the previous typed column:
    /// `[["Header-Name", "value"], ...]`.
    #[serde(default)]
    pub register_extra_headers: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub rewrite_hostport: bool,
    #[serde(default)]
    pub did_numbers: Option<Value>,
    #[serde(default)]
    pub incoming_from_user_prefix: Option<String>,
    #[serde(default)]
    pub incoming_to_user_prefix: Option<String>,
    #[serde(default)]
    pub default_route_label: Option<String>,
    #[serde(default)]
    pub billing_snapshot: Option<Value>,
    #[serde(default)]
    pub analytics: Option<Value>,
    #[serde(default)]
    pub carrier: Option<String>,
}

impl SipTrunkConfig {
    /// Validate SIP-specific config. The shared fields (name, max_cps, ...)
    /// are validated separately by the CRUD layer.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(s) = &self.sip_server
            && s.trim().is_empty()
        {
            return Err(ValidationError::custom("sip_server must not be empty"));
        }
        if let Some(expires) = self.register_expires
            && expires <= 0
        {
            return Err(ValidationError::custom(
                "register_expires must be > 0 when set",
            ));
        }
        Ok(())
    }
}

fn default_audio_codec() -> String {
    "opus".to_string()
}

/// Configuration shape for `kind = "webrtc"` trunks. Serialized as JSON in
/// the `trunks.kind_config` column.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WebRtcTrunkConfig {
    /// Signaling adapter name (e.g. `"http_json"`).
    pub signaling: String,
    /// HTTP endpoint of the WebRTC signaling peer.
    pub endpoint_url: String,
    /// Optional ICE-server list passed to the outbound `PeerConnection`.
    /// Falls back to the global `[ice_servers]` config if omitted.
    #[serde(default)]
    pub ice_servers: Option<Value>,
    /// Audio codec on the WebRTC side. Validated against the allow-list.
    #[serde(default = "default_audio_codec")]
    pub audio_codec: String,
    /// Optional `Authorization` header forwarded to the signaling endpoint.
    #[serde(default)]
    pub auth_header: Option<String>,
    /// Adapter-specific protocol blob. Shape is validated by the matching
    /// `WebRtcSignalingAdapter::validate_protocol`.
    #[serde(default)]
    pub protocol: Option<Value>,
}

impl WebRtcTrunkConfig {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.signaling.trim().is_empty() {
            return Err(ValidationError::custom("signaling must not be empty"));
        }
        url::Url::parse(&self.endpoint_url)
            .map_err(|e| ValidationError::custom(format!("endpoint_url is not a valid URL: {e}")))?;
        match self.audio_codec.as_str() {
            "opus" | "g722" => {}
            other => {
                return Err(ValidationError::custom(format!(
                    "audio_codec '{other}' not supported (allowed: opus, g722)"
                )));
            }
        }
        if let Some(ice) = &self.ice_servers
            && !ice.is_array()
        {
            return Err(ValidationError::custom(
                "ice_servers, if present, must be a JSON array",
            ));
        }
        Ok(())
    }
}
