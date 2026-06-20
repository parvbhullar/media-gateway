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
use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, QueryFilter};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Deserialize a JSON bool tolerantly: accepts `true`/`false`, `0`/`1`, or
/// the string forms `"true"`/`"false"`/`"0"`/`"1"`. Needed because the
/// initial unify migration (`migrate_sip_trunks_to_trunks_unified`) packed
/// SQLite BOOLEAN columns into `kind_config` JSON via `json_object(...)`,
/// which emits the raw integer 0/1 rather than a JSON boolean. The
/// follow-up migration `fix_sip_trunk_kind_config_booleans` repairs the
/// stored data, but this deserializer keeps us safe against future imports
/// or migrations producing the same shape.
fn bool_from_anything<'de, D>(de: D) -> std::result::Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    let v = Value::deserialize(de)?;
    match &v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i != 0)
            .ok_or_else(|| D::Error::custom(format!("invalid bool number: {n}"))),
        Value::String(s) => match s.as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            other => Err(D::Error::custom(format!("invalid bool string: {other}"))),
        },
        Value::Null => Ok(false),
        other => Err(D::Error::custom(format!(
            "invalid bool value: {other}"
        ))),
    }
}

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
    /// Owning org (task 3.1 tenant isolation). DB default `'default'` until 3.1b
    /// threads the real org_id from request context.
    pub org_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// List trunks owned by `org_id` (task 3.1 tenant isolation).
    pub async fn list_by_org(
        db: &DatabaseConnection,
        org_id: &str,
    ) -> std::result::Result<Vec<Self>, DbErr> {
        Entity::find().filter(Column::OrgId.eq(org_id)).all(db).await
    }

    /// Get a trunk by name, scoped to `org_id` — `None` when the trunk belongs
    /// to a different org (cross-tenant lookups never resolve).
    pub async fn get_by_org(
        db: &DatabaseConnection,
        org_id: &str,
        name: &str,
    ) -> std::result::Result<Option<Self>, DbErr> {
        Entity::find()
            .filter(Column::Name.eq(name))
            .filter(Column::OrgId.eq(org_id))
            .one(db)
            .await
    }

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

    /// Typed view of this row's `kind_config` as a `LiveKitTrunkConfig`.
    /// Errors if `kind != "livekit"` or the JSON does not match the schema.
    pub fn livekit(&self) -> Result<LiveKitTrunkConfig> {
        ensure!(
            self.kind == "livekit",
            "kind mismatch: expected 'livekit', got '{}'",
            self.kind
        );
        serde_json::from_value(self.kind_config.clone())
            .map_err(|e| anyhow!("failed to deserialize LiveKitTrunkConfig from kind_config: {e}"))
    }

    /// Typed view of this row's `kind_config` as an `ExternalMediaTrunkConfig`.
    /// Errors if `kind != "external_media"` or the JSON does not match the schema.
    pub fn external_media(&self) -> Result<ExternalMediaTrunkConfig> {
        ensure!(
            self.kind == "external_media",
            "kind mismatch: expected 'external_media', got '{}'",
            self.kind
        );
        serde_json::from_value(self.kind_config.clone()).map_err(|e| {
            anyhow!("failed to deserialize ExternalMediaTrunkConfig from kind_config: {e}")
        })
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
    #[serde(default, deserialize_with = "bool_from_anything")]
    pub register_enabled: bool,
    #[serde(default)]
    pub register_expires: Option<i32>,
    /// Matches the JSON shape used in the previous typed column:
    /// `[["Header-Name", "value"], ...]`.
    #[serde(default)]
    pub register_extra_headers: Option<Vec<(String, String)>>,
    #[serde(default, deserialize_with = "bool_from_anything")]
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

/// Serde helper for fields whose default value is `true`.
fn default_true() -> bool {
    true
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
    /// Optional override URL used by the gateway-health probe (HTTP HEAD).
    /// When set, the prober hits this URL instead of `endpoint_url`. Useful
    /// when the signaling endpoint only handles `POST` (returns 405 on
    /// HEAD) and the bot exposes a separate `/healthz` or `/` for
    /// liveness. Falls back to `endpoint_url` when unset.
    #[serde(default)]
    pub health_check_url: Option<String>,
    /// Adapter-specific protocol blob. Shape is validated by the matching
    /// `WebRtcSignalingAdapter::validate_protocol`.
    #[serde(default)]
    pub protocol: Option<Value>,
    /// Per-trunk signaling timeout in milliseconds. Defaults to 5000 when
    /// unset. Bots that need cold-start time (model load, container spin-up)
    /// can raise this without touching global config.
    #[serde(default)]
    pub signaling_timeout_ms: Option<u64>,
}

impl WebRtcTrunkConfig {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.signaling.trim().is_empty() {
            return Err(ValidationError::custom("signaling must not be empty"));
        }
        url::Url::parse(&self.endpoint_url)
            .map_err(|e| ValidationError::custom(format!("endpoint_url is not a valid URL: {e}")))?;
        if let Some(hc) = &self.health_check_url {
            url::Url::parse(hc).map_err(|e| {
                ValidationError::custom(format!("health_check_url is not a valid URL: {e}"))
            })?;
        }
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

/// Configuration shape for `kind = "livekit"` trunks. Serialized as JSON in
/// the `trunks.kind_config` column. See spec:
/// `docs/superpowers/specs/2026-05-25-livekit-trunk-design.md`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LiveKitTrunkConfig {
    /// LiveKit signaling URL — ws:// or wss://
    pub server_url: String,
    /// LiveKit API key.
    pub api_key: String,
    /// LiveKit API secret (used for JWT mint + admin RPCs).
    pub api_secret: String,
    /// Templated room name. Supported vars: {call_id} {from_user} {to_user}
    /// {did} {trunk_name}.
    pub room_template: String,
    /// Templated participant identity. Same supported vars.
    pub identity_template: String,
    /// Optional templated metadata. Must be valid JSON after substitution.
    #[serde(default)]
    pub metadata_template: Option<String>,
    /// Audio codec on the LiveKit publish track. Validated against the
    /// allow-list (currently: opus, g722).
    #[serde(default = "default_audio_codec")]
    pub audio_codec: String,
    /// Optional HTTP webhook called before the LiveKit room connect. The
    /// response follows the Decision API contract — see spec.
    #[serde(default)]
    pub dispatch_endpoint: Option<String>,
    /// Optional Authorization header for the webhook.
    #[serde(default)]
    pub dispatch_endpoint_auth_header: Option<String>,
    /// Optional adapter-specific protocol blob for the webhook — mirrors
    /// the WebRtc HttpJsonProtocol shape; supports request_body_template.
    #[serde(default)]
    pub dispatch_endpoint_protocol: Option<Value>,
    /// If true (default), schema-validation OR transport failures on
    /// the webhook abort the call. If false, transient failures fall
    /// back to "accept with no overrides" — schema failures still always
    /// abort regardless. Default `true` because production deployments
    /// want hard-fail on controller bugs / outages to surface in CDR
    /// and metrics instead of silently dropping callers into rooms.
    #[serde(default = "default_true")]
    pub require_webhook_ack: bool,
    /// Optional URL for kind-aware health probing. Falls back to TCP-connect
    /// against `server_url`'s host:port when unset.
    #[serde(default)]
    pub health_check_url: Option<String>,
    /// Per-trunk signaling timeout in milliseconds. Defaults to 5000.
    /// Applies to both the optional webhook POST and the LiveKit
    /// Room::connect handshake.
    #[serde(default)]
    pub signaling_timeout_ms: Option<u64>,
    /// If true, when the SIP call ends we additionally call the LiveKit
    /// RoomService::delete_room admin RPC. Useful for single-use rooms.
    /// Default false: leave the room alive for other participants.
    #[serde(default)]
    pub delete_room_on_hangup: bool,
    /// LiveKit "explicit agent dispatch" — when set, after our caller
    /// participant joins the room we call the LiveKit AgentDispatch
    /// admin RPC to spin the named agent into the same room. Lets you
    /// run with zero external controller: rustpbx itself triggers the
    /// bot. At least one of `agent_name` or `dispatch_endpoint` MUST be
    /// set (otherwise the caller lands in an empty room and hears
    /// silence forever).
    #[serde(default)]
    pub agent_name: Option<String>,
    /// When true (default), a failure of the LiveKit AgentDispatch RPC
    /// aborts the call with 503 instead of soft-fail-and-continue.
    /// Pairs with `agent_name`. Default `true` so misconfigured agents
    /// surface as CDR failures + tracing warnings rather than silent
    /// empty rooms.
    #[serde(default = "default_true")]
    pub require_agent_dispatch: bool,
    /// When set, the LiveKit bridge runs a watchdog after Room::connect:
    /// if no remote participant has TrackSubscribed within this many
    /// milliseconds, the bridge cancels itself, which triggers the
    /// SIP-side teardown (CDR `hangup=ByCallee`, `last_error="bot did
    /// not join within …"`). Closes the runtime "silent empty room"
    /// gap that agent_name + webhook alone can't guarantee — the
    /// configured agent might fail to register on the LiveKit side, the
    /// webhook controller might silently drop the request, etc.
    /// Default None = no watchdog (call stays up until SIP-side BYE).
    #[serde(default)]
    pub bot_join_timeout_ms: Option<u64>,
    /// When set, the LiveKit bridge emits a low-amplitude sine tone at
    /// this frequency on the SIP-RTP side whenever `subscribers` is
    /// empty (no remote participant publishing audio). Replaces the
    /// default silence so the caller hears *something* while waiting
    /// for the bot to land. Recommended values: 0 = silence (default),
    /// 200-440 = "please hold" tone. Frequencies above ~3400 Hz are
    /// clipped by the SIP codec (PCMU/PCMA 8 kHz Nyquist).
    #[serde(default)]
    pub hold_tone_hz: Option<u16>,
    /// JWT TTL in seconds (default 1800 = 30 min). Controls how long
    /// the minted access token can authenticate `Room::connect` —
    /// once a participant is in the room the TTL no longer matters
    /// for the live session, BUT the LiveKit SDK's auto-reconnect
    /// path re-uses the original JWT, so a network blip on a call
    /// that's been up longer than the TTL fails to recover. Raise
    /// for long-running calls; lower for tighter token hygiene.
    #[serde(default)]
    pub jwt_ttl_secs: Option<u64>,
}

impl LiveKitTrunkConfig {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.server_url.trim().is_empty() {
            return Err(ValidationError::custom("server_url must not be empty"));
        }
        if !(self.server_url.starts_with("ws://") || self.server_url.starts_with("wss://")) {
            return Err(ValidationError::custom(
                "server_url must be ws:// or wss://",
            ));
        }
        if self.api_key.trim().is_empty() {
            return Err(ValidationError::custom("api_key must not be empty"));
        }
        if self.api_secret.trim().is_empty() {
            return Err(ValidationError::custom("api_secret must not be empty"));
        }
        if self.room_template.trim().is_empty() {
            return Err(ValidationError::custom("room_template must not be empty"));
        }
        if self.identity_template.trim().is_empty() {
            return Err(ValidationError::custom("identity_template must not be empty"));
        }
        // At least one of agent_name OR dispatch_endpoint must be set, so
        // there's a way for the bot to be told about the call. Both unset
        // leaves the caller in an empty room hearing silence (see
        // `docs/smoke_test_livekit_bridge.md`).
        let has_agent = self
            .agent_name
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_webhook = self
            .dispatch_endpoint
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !has_agent && !has_webhook {
            return Err(ValidationError::custom(
                "at least one of agent_name or dispatch_endpoint must be set \
                 — otherwise the caller will join an empty LiveKit room with \
                 nobody to hear them",
            ));
        }
        match self.audio_codec.as_str() {
            "opus" | "g722" => {}
            other => {
                return Err(ValidationError::custom(format!(
                    "audio_codec '{other}' not supported (allowed: opus, g722)"
                )));
            }
        }
        if let Some(md) = &self.metadata_template {
            // Validate that after substituting placeholder values, the
            // template parses as JSON. Replace known {var} placeholders
            // with a harmless JSON-string value before parsing.
            let probe = md
                .replace("{call_id}", "_")
                .replace("{from_user}", "_")
                .replace("{to_user}", "_")
                .replace("{did}", "_")
                .replace("{trunk_name}", "_");
            serde_json::from_str::<Value>(&probe).map_err(|e| {
                ValidationError::custom(format!(
                    "metadata_template must be valid JSON after substitution: {e}"
                ))
            })?;
        }
        if let Some(url) = &self.health_check_url {
            url::Url::parse(url).map_err(|e| {
                ValidationError::custom(format!("health_check_url not a URL: {e}"))
            })?;
        }
        if let Some(url) = &self.dispatch_endpoint {
            url::Url::parse(url).map_err(|e| {
                ValidationError::custom(format!("dispatch_endpoint not a URL: {e}"))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod livekit_config_tests {
    use super::*;

    fn base() -> LiveKitTrunkConfig {
        // base() now includes agent_name so validate() passes — the
        // bot-routing requirement (`agent_name` OR `dispatch_endpoint`)
        // is exercised by dedicated tests below.
        LiveKitTrunkConfig {
            server_url: "wss://livekit.example.com".to_string(),
            api_key: "key".to_string(),
            api_secret: "secret".to_string(),
            room_template: "room-{call_id}".to_string(),
            identity_template: "sip-{from_user}".to_string(),
            metadata_template: None,
            audio_codec: "opus".to_string(),
            dispatch_endpoint: None,
            dispatch_endpoint_auth_header: None,
            dispatch_endpoint_protocol: None,
            require_webhook_ack: false,
            health_check_url: None,
            signaling_timeout_ms: None,
            delete_room_on_hangup: false,
            agent_name: Some("voice-agent".to_string()),
            require_agent_dispatch: false,
            bot_join_timeout_ms: None,
            hold_tone_hz: None,
            jwt_ttl_secs: None,
        }
    }

    #[test]
    fn rejects_empty_server_url() {
        let mut cfg = base();
        cfg.server_url = "".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_non_ws_scheme() {
        let mut cfg = base();
        cfg.server_url = "https://example.com".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_codec() {
        let mut cfg = base();
        cfg.audio_codec = "pcmu".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_invalid_metadata_json() {
        let mut cfg = base();
        cfg.metadata_template = Some("{not-json".to_string());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_valid_config() {
        let cfg = base();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn accepts_valid_metadata_template_with_vars() {
        let mut cfg = base();
        cfg.metadata_template =
            Some(r#"{"call":"{call_id}","from":"{from_user}"}"#.to_string());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_when_neither_agent_name_nor_dispatch_endpoint_set() {
        let mut cfg = base();
        cfg.agent_name = None;
        cfg.dispatch_endpoint = None;
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("agent_name") && err.contains("dispatch_endpoint"),
            "expected error to mention both fields, got: {err}"
        );
    }

    #[test]
    fn accepts_when_only_dispatch_endpoint_set() {
        let mut cfg = base();
        cfg.agent_name = None;
        cfg.dispatch_endpoint = Some("https://controller.example.com/dispatch".to_string());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn accepts_when_only_agent_name_set() {
        let mut cfg = base();
        cfg.dispatch_endpoint = None;
        // base() already has agent_name = Some("voice-agent")
        assert!(cfg.agent_name.is_some());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_when_agent_name_is_whitespace_only() {
        let mut cfg = base();
        cfg.agent_name = Some("   ".to_string());
        cfg.dispatch_endpoint = None;
        assert!(cfg.validate().is_err(), "whitespace-only agent_name must not satisfy the gate");
    }
}

/// Configuration shape for `kind = "external_media"` trunks. rustpbx
/// terminates SIP + RTP and pipes decoded 48 kHz mono PCM to a co-located
/// sidecar process over a localhost UDP socket; the sidecar owns the
/// far-end media logic (e.g. joining a LiveKit room as a participant).
/// See `docs/superpowers/specs/2026-05-29-external-media-bridge-design.md`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExternalMediaTrunkConfig {
    /// Command used to spawn the per-call sidecar. rustpbx appends
    /// `--call-id <id> --did <to_user> --caller <from_user> --port <P>`.
    /// Example: `python3 /opt/livekit/participant.py`.
    pub command: String,
    /// SIP-side voice codec to negotiate (drives rustpbx's transcoder).
    /// Allowed: opus, g722, pcmu, pcma. Default opus.
    #[serde(default = "default_audio_codec")]
    pub audio_codec: String,
    /// How long (ms) to hold the SIP INVITE in 100 Trying waiting for the
    /// sidecar's `READY` datagram before answering. On timeout the call is
    /// failed (503). Default 15000.
    #[serde(default)]
    pub bot_join_timeout_ms: Option<u64>,
    /// Optional low-amplitude sine tone (Hz) played to the SIP caller while
    /// no sidecar audio is arriving yet. Default none (silence).
    #[serde(default)]
    pub hold_tone_hz: Option<u16>,
}

impl ExternalMediaTrunkConfig {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.command.trim().is_empty() {
            return Err(ValidationError::custom("command must not be empty"));
        }
        match self.audio_codec.as_str() {
            "opus" | "g722" | "pcmu" | "pcma" => {}
            other => {
                return Err(ValidationError::custom(format!(
                    "audio_codec '{other}' not supported (allowed: opus, g722, pcmu, pcma)"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod org_id_tests {
    use super::*;
    use crate::models::migration::Migrator;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, ActiveValue::Set, Database};
    use sea_orm_migration::MigratorTrait;

    async fn fresh() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("sqlite");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }

    fn row(name: &str, org: Option<&str>) -> ActiveModel {
        let now = Utc::now();
        let mut am = ActiveModel {
            name: Set(name.to_string()),
            kind: Set("sip".to_string()),
            status: Set(TrunkStatus::default()),
            direction: Set(TrunkDirection::default()),
            is_active: Set(true),
            consecutive_failures: Set(0),
            consecutive_successes: Set(0),
            kind_config: Set(serde_json::json!({})),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        // Leave org_id NotSet when None so the column default ('default') applies.
        if let Some(org) = org {
            am.org_id = Set(org.to_string());
        }
        am
    }

    #[tokio::test]
    async fn list_by_org_isolates_tenants() {
        let db = fresh().await;
        row("trunk_a", Some("org_a")).insert(&db).await.expect("insert a");
        row("trunk_b", Some("org_b")).insert(&db).await.expect("insert b");
        let a = Model::list_by_org(&db, "org_a").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "trunk_a");
        assert_eq!(a[0].org_id, "org_a");
    }

    #[tokio::test]
    async fn get_by_org_rejects_foreign_tenant() {
        let db = fresh().await;
        row("trunk_a", Some("org_a")).insert(&db).await.expect("insert a");
        // The trunk exists but belongs to org_a → a cross-tenant get is None.
        assert!(
            Model::get_by_org(&db, "org_b", "trunk_a")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            Model::get_by_org(&db, "org_a", "trunk_a")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn insert_defaults_org_id() {
        // org_id NotSet → the column default ('default') applies, so existing
        // write paths keep working until 3.1b threads a real org_id.
        let db = fresh().await;
        row("trunk_c", None).insert(&db).await.expect("insert c");
        let found = Model::get_by_org(&db, "default", "trunk_c")
            .await
            .unwrap()
            .expect("row under default org");
        assert_eq!(found.org_id, "default");
    }
}
