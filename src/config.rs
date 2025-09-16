use crate::{
    call::user::SipUser,
    proxy::routing::{DefaultRoute, RouteRule, TrunkConfig},
    useragent::RegisterOption,
    transcription::{TranscriptionOption, TranscriptionType},
    synthesis::{SynthesisOption, SynthesisType},
};
use anyhow::{Error, Result};
use clap::Parser;
use rsipstack::dialog::invitation::InviteOption;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const USER_AGENT: &str = "rustpbx";

#[derive(Parser, Debug)]
#[command(version)]
pub(crate) struct Cli {
    #[clap(long, default_value = "rustpbx.toml")]
    pub conf: Option<String>,
}

fn default_config_recorder_path() -> String {
    #[cfg(target_os = "windows")]
    return "./recorder".to_string();
    #[cfg(not(target_os = "windows"))]
    return "/tmp/recorder".to_string();
}
fn default_config_media_cache_path() -> String {
    #[cfg(target_os = "windows")]
    return "./mediacache".to_string();
    #[cfg(not(target_os = "windows"))]
    return "/tmp/mediacache".to_string();
}
fn default_config_http_addr() -> String {
    "0.0.0.0:8080".to_string()
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_config_http_addr")]
    pub http_addr: String,
    pub log_level: Option<String>,
    pub log_file: Option<String>,
    pub ua: Option<UseragentConfig>,
    pub proxy: Option<ProxyConfig>,
    #[serde(default = "default_config_recorder_path")]
    pub recorder_path: String,
    pub callrecord: Option<CallRecordConfig>,
    #[serde(default = "default_config_media_cache_path")]
    pub media_cache_path: String,
    pub llmproxy: Option<String>,
    pub restsend_token: Option<String>,
    pub ice_servers: Option<Vec<IceServer>>,
    pub ami: Option<AmiConfig>,
    /// Deepgram API key for ASR and TTS
    pub deepgram_api_key: Option<String>,
    /// Structured Deepgram configuration
    pub deepgram: Option<DeepgramConfig>,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct UseragentConfig {
    pub addr: String,
    pub udp_port: u16,
    pub external_ip: Option<String>,
    pub stun_server: Option<String>,
    pub rtp_start_port: Option<u16>,
    pub rtp_end_port: Option<u16>,
    pub useragent: Option<String>,
    pub callid_suffix: Option<String>,
    pub register_users: Option<Vec<RegisterOption>>,
    pub graceful_shutdown: Option<bool>,
    pub handler: Option<InviteHandlerConfig>,
    pub accept_timeout: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum InviteHandlerConfig {
    Webhook {
        url: String,
        method: Option<String>,
        headers: Option<Vec<(String, String)>>,
    },
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum UserBackendConfig {
    Memory {
        users: Option<Vec<SipUser>>,
    },
    Http {
        url: String,
        method: Option<String>,
        username_field: Option<String>,
        realm_field: Option<String>,
        headers: Option<HashMap<String, String>>,
    },
    Plain {
        path: String,
    },
    Database {
        url: String,
        table_name: Option<String>,
        id_column: Option<String>,
        username_column: Option<String>,
        password_column: Option<String>,
        enabled_column: Option<String>,
        realm_column: Option<String>,
    },
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum LocatorConfig {
    Memory,
    Http {
        url: String,
        method: Option<String>,
        username_field: Option<String>,
        expires_field: Option<String>,
        realm_field: Option<String>,
        headers: Option<HashMap<String, String>>,
    },
    Database {
        url: String,
    },
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum S3Vendor {
    Aliyun,
    Tencent,
    Minio,
    AWS,
    GCP,
    Azure,
    DigitalOcean,
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum CallRecordConfig {
    Local {
        root: String,
    },
    S3 {
        vendor: S3Vendor,
        bucket: String,
        region: String,
        access_key: String,
        secret_key: String,
        endpoint: String,
        root: String,
        with_media: Option<bool>,
    },
    Http {
        url: String,
        headers: Option<HashMap<String, String>>,
        with_media: Option<bool>,
    },
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(PartialEq)]
pub enum MediaProxyMode {
    /// All media goes through proxy
    All,
    /// Auto detect if media proxy is needed (webrtc to rtp)
    Auto,
    /// Only handle NAT (private IP addresses)
    Nat,
    /// Do not handle media proxy
    None,
}

impl Default for MediaProxyMode {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct MediaProxyConfig {
    pub mode: MediaProxyMode,
    pub rtp_start_port: Option<u16>,
    pub rtp_end_port: Option<u16>,
    pub external_ip: Option<String>,
    pub force_proxy: Option<Vec<String>>, // List of IP addresses to always proxy
}

impl Default for MediaProxyConfig {
    fn default() -> Self {
        Self {
            mode: MediaProxyMode::None,
            rtp_start_port: Some(20000),
            rtp_end_port: Some(30000),
            external_ip: None,
            force_proxy: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProxyConfig {
    pub modules: Option<Vec<String>>,
    pub addr: String,
    pub external_ip: Option<String>,
    pub useragent: Option<String>,
    pub callid_suffix: Option<String>,
    pub ssl_private_key: Option<String>,
    pub ssl_certificate: Option<String>,
    pub udp_port: Option<u16>,
    pub tcp_port: Option<u16>,
    pub tls_port: Option<u16>,
    pub ws_port: Option<u16>,
    pub acl_rules: Option<Vec<String>>,
    pub max_concurrency: Option<usize>,
    pub registrar_expires: Option<u32>,
    #[serde(default)]
    pub user_backend: UserBackendConfig,
    #[serde(default)]
    pub locator: LocatorConfig,
    #[serde(default)]
    pub media_proxy: MediaProxyConfig,
    #[serde(default)]
    pub realms: Option<Vec<String>>,
    pub ws_handler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<RouteRule>>,
    #[serde(default)]
    pub trunks: HashMap<String, TrunkConfig>,
    #[serde(default)]
    pub default: Option<DefaultRoute>,
}

pub enum RouteResult {
    Forward(InviteOption),
    Abort(u16, String),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AmiConfig {
    pub allows: Option<Vec<String>>,
}

impl AmiConfig {
    pub fn is_allowed(&self, addr: &str) -> bool {
        if let Some(allows) = &self.allows {
            allows.iter().any(|a| a == addr || a == "*")
        } else {
            false
        }
    }
}

impl Default for AmiConfig {
    fn default() -> Self {
        Self {
            allows: Some(vec!["127.0.0.1".to_string(), "::1".to_string()]), // Default to allow localhost
        }
    }
}

impl ProxyConfig {
    pub fn normalize_realm(realm: &str) -> &str {
        if realm.is_empty() || realm == "*" || realm == "127.0.0.1" || realm == "::1" {
            "localhost"
        } else {
            realm
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            acl_rules: Some(vec!["allow all".to_string(), "deny all".to_string()]),
            addr: "0.0.0.0".to_string(),
            modules: Some(vec![
                "acl".to_string(),
                "auth".to_string(),
                "registrar".to_string(),
                "call".to_string(),
            ]),
            external_ip: None,
            useragent: None,
            callid_suffix: Some("restsend.com".to_string()),
            ssl_private_key: None,
            ssl_certificate: None,
            udp_port: Some(5060),
            tcp_port: None,
            tls_port: None,
            ws_port: None,
            max_concurrency: None,
            registrar_expires: Some(60),
            user_backend: UserBackendConfig::default(),
            locator: LocatorConfig::default(),
            media_proxy: MediaProxyConfig::default(),
            realms: Some(vec![]),
            ws_handler: None,
            routes: None,
            trunks: HashMap::new(),
            default: None,
        }
    }
}

impl Default for UserBackendConfig {
    fn default() -> Self {
        Self::Memory { users: None }
    }
}

impl Default for LocatorConfig {
    fn default() -> Self {
        Self::Memory
    }
}

impl Default for UseragentConfig {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0".to_string(),
            udp_port: 25060,
            external_ip: None,
            rtp_start_port: Some(12000),
            rtp_end_port: Some(42000),
            stun_server: None,
            useragent: Some(USER_AGENT.to_string()),
            callid_suffix: Some("restsend.com".to_string()),
            register_users: None,
            graceful_shutdown: Some(true),
            handler: None,
            accept_timeout: Some("50s".to_string()),
        }
    }
}

impl Default for CallRecordConfig {
    fn default() -> Self {
        Self::Local {
            #[cfg(target_os = "windows")]
            root: "./cdr".to_string(),
            #[cfg(not(target_os = "windows"))]
            root: "/tmp/cdr".to_string(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            http_addr: default_config_http_addr(),
            log_level: None,
            log_file: None,
            ua: Some(UseragentConfig::default()),
            proxy: None,
            recorder_path: default_config_recorder_path(),
            media_cache_path: default_config_media_cache_path(),
            callrecord: None,
            llmproxy: None,
            restsend_token: None,
            ice_servers: None,
            ami: Some(AmiConfig::default()),
            deepgram_api_key: None,
            deepgram: None,
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Error> {
        let config = toml::from_str(
            &std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {}", e, path))?,
        )?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_dump() {
        let mut config = Config::default();
        let mut prxconfig = ProxyConfig::default();
        let mut trunks = HashMap::new();
        let mut routes = Vec::new();
        let mut ice_servers = Vec::new();
        ice_servers.push(IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            username: Some("user".to_string()),
            ..Default::default()
        });
        ice_servers.push(IceServer {
            urls: vec![
                "stun:restsend.com:3478".to_string(),
                "turn:stun.l.google.com:1112?transport=TCP".to_string(),
            ],
            username: Some("user".to_string()),
            ..Default::default()
        });

        routes.push(crate::proxy::routing::RouteRule {
            name: "default".to_string(),
            description: None,
            priority: 1,
            match_conditions: crate::proxy::routing::MatchConditions {
                to_user: Some("xx".to_string()),
                ..Default::default()
            },
            rewrite: Some(crate::proxy::routing::RewriteRules {
                to_user: Some("xx".to_string()),
                ..Default::default()
            }),
            action: crate::proxy::routing::RouteAction::default(),
            disabled: None,
        });
        routes.push(crate::proxy::routing::RouteRule {
            name: "default3".to_string(),
            description: None,
            priority: 1,
            match_conditions: crate::proxy::routing::MatchConditions {
                to_user: Some("xx3".to_string()),
                ..Default::default()
            },
            rewrite: Some(crate::proxy::routing::RewriteRules {
                to_user: Some("xx3".to_string()),
                ..Default::default()
            }),
            action: crate::proxy::routing::RouteAction::default(),
            disabled: None,
        });
        prxconfig.routes = Some(routes);
        trunks.insert(
            "hello".to_string(),
            crate::proxy::routing::TrunkConfig {
                dest: "sip:127.0.0.1:5060".to_string(),
                ..Default::default()
            },
        );
        prxconfig.trunks = trunks;
        config.proxy = Some(prxconfig);
        config.ice_servers = Some(ice_servers);
        let config_str = toml::to_string(&config).unwrap();
        println!("{}", config_str);
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeepgramConfig {
    /// Deepgram API key (can also be set via DEEPGRAM_API_KEY env var)
    pub api_key: Option<String>,
    /// ASR configuration
    pub asr: Option<DeepgramAsrConfig>,
    /// TTS configuration
    pub tts: Option<DeepgramTtsConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeepgramAsrConfig {
    /// Model to use for ASR (default: nova-2-general)
    pub model: Option<String>,
    /// Language code (default: en)
    pub language: Option<String>,
    /// Audio encoding (default: linear16)
    pub encoding: Option<String>,
    /// Sample rate (default: 16000)
    pub sample_rate: Option<u32>,
    /// Number of channels (default: 1)
    pub channels: Option<u32>,
    /// Enable interim results (default: true)
    pub interim_results: Option<bool>,
    /// Enable punctuation (default: true)
    pub punctuate: Option<bool>,
    /// Enable smart formatting (default: true)
    pub smart_format: Option<bool>,
    /// Endpointing timeout in milliseconds (default: 1000)
    pub endpointing: Option<u32>,
    /// Enable VAD events (default: true)
    pub vad_events: Option<bool>,
    /// Custom endpoint URL
    pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeepgramTtsConfig {
    /// Voice model to use (default: aura-asteria-en)
    pub model: Option<String>,
    /// Audio encoding (default: linear16)
    pub encoding: Option<String>,
    /// Sample rate (default: 16000)
    pub sample_rate: Option<u32>,
    /// Custom endpoint URL
    pub endpoint: Option<String>,
}

impl Default for DeepgramConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            asr: Some(DeepgramAsrConfig::default()),
            tts: Some(DeepgramTtsConfig::default()),
        }
    }
}

impl Default for DeepgramAsrConfig {
    fn default() -> Self {
        Self {
            model: Some("nova".to_string()),
            language: Some("en".to_string()),
            encoding: Some("linear16".to_string()),
            sample_rate: Some(16000),
            channels: Some(1),
            interim_results: Some(true),
            punctuate: Some(true),
            smart_format: Some(true),
            endpointing: Some(1000),
            vad_events: Some(true),
            endpoint: None,
        }
    }
}

impl Default for DeepgramTtsConfig {
    fn default() -> Self {
        Self {
            model: Some("aura-asteria-en".to_string()),
            encoding: Some("linear16".to_string()),
            sample_rate: Some(16000),
            endpoint: None,
        }
    }
}

impl Config {
    /// Create a default Deepgram TranscriptionOption from config
    pub fn create_deepgram_asr_option(&self) -> Option<TranscriptionOption> {
        // Check if we have Deepgram API key (from structured config or legacy field)
        let api_key = self.deepgram.as_ref()
            .and_then(|dg| dg.api_key.as_ref())
            .or(self.deepgram_api_key.as_ref())
            .cloned()
            .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok());

        if api_key.is_some() {
            let asr_config = self.deepgram.as_ref()
                .and_then(|dg| dg.asr.as_ref())
                .cloned()
                .unwrap_or_default();

            Some(TranscriptionOption {
                provider: Some(TranscriptionType::Deepgram),
                secret_key: api_key,
                language: asr_config.language,
                model_type: asr_config.model,
                samplerate: asr_config.sample_rate,
                endpoint: asr_config.endpoint,
                ..Default::default()
            })
        } else {
            None
        }
    }

    /// Create a default Deepgram SynthesisOption from config
    pub fn create_deepgram_tts_option(&self) -> Option<SynthesisOption> {
        // Check if we have Deepgram API key (from structured config or legacy field)
        let api_key = self.deepgram.as_ref()
            .and_then(|dg| dg.api_key.as_ref())
            .or(self.deepgram_api_key.as_ref())
            .cloned()
            .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok());

        if api_key.is_some() {
            let tts_config = self.deepgram.as_ref()
                .and_then(|dg| dg.tts.as_ref())
                .cloned()
                .unwrap_or_default();

            Some(SynthesisOption {
                provider: Some(SynthesisType::Deepgram),
                secret_key: api_key,
                speaker: tts_config.model,
                codec: tts_config.encoding,
                samplerate: tts_config.sample_rate.map(|r| r as i32),
                endpoint: tts_config.endpoint,
                ..Default::default()
            })
        } else {
            None
        }
    }
}
