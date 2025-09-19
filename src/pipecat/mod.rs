/*!
 * Pipecat Media Server Integration for RustPBX
 * 
 * This module provides integration with the Pipecat media server for AI processing.
 * When enabled, audio streams are forwarded to the Pipecat server for STT, LLM, and TTS processing
 * instead of using the internal AI services.
 */

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub mod client;
pub mod config;

pub use client::PipecatClient;
pub use config::PipecatConfig;

/// Pipecat server connection status
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

/// Audio frame for Pipecat processing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipecatAudioFrame {
    pub audio_data: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub timestamp: u64,
    pub frame_id: String,
}

/// Response from Pipecat server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PipecatResponse {
    #[serde(rename = "audio")]
    Audio {
        audio_data: Vec<u8>,
        sample_rate: u32,
        channels: u32,
        frame_id: String,
    },
    #[serde(rename = "transcription")]
    Transcription {
        text: String,
        is_final: bool,
        timestamp: u64,
        language: String,
    },
    #[serde(rename = "llm_response")]
    LlmResponse {
        text: String,
        is_complete: bool,
        timestamp: u64,
    },
    #[serde(rename = "tts_started")]
    TtsStarted {
        text: String,
        timestamp: u64,
    },
    #[serde(rename = "tts_completed")]
    TtsCompleted {
        text: String,
        timestamp: u64,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        code: Option<i32>,
        timestamp: u64,
    },
    #[serde(rename = "metrics")]
    Metrics {
        key: String,
        duration: u64,
        timestamp: u64,
    },
    #[serde(rename = "ping")]
    Ping {
        timestamp: u64,
    },
    #[serde(rename = "pong")]
    Pong {
        timestamp: u64,
    },
    #[serde(rename = "connected")]
    Connected {
        server: String,
        version: String,
        timestamp: u64,
    },
    #[serde(rename = "configured")]
    Configured {
        call_id: String,
        status: String,
        timestamp: u64,
    },
}

/// WebSocket message wrapper for Pipecat communication
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command")]
pub enum PipecatMessage {
    #[serde(rename = "audio")]
    Audio(PipecatAudioFrame),
    #[serde(rename = "configure")]
    Configure {
        room_id: String,
        system_prompt: Option<String>,
        stt_config: Option<serde_json::Value>,
        llm_config: Option<serde_json::Value>,
        tts_config: Option<serde_json::Value>,
    },
    #[serde(rename = "ping")]
    Ping {
        timestamp: u64,
    },
    #[serde(rename = "disconnect")]
    Disconnect {
        reason: String,
    },
}

/// Event sent to RustPBX from Pipecat processing
#[derive(Debug, Clone)]
pub enum PipecatEvent {
    TranscriptionDelta {
        text: String,
        timestamp: u64,
    },
    TranscriptionFinal {
        text: String,
        timestamp: u64,
    },
    LlmResponse {
        text: String,
        is_complete: bool,
        timestamp: u64,
    },
    AudioResponse {
        audio_data: Vec<u8>,
        sample_rate: u32,
        channels: u32,
    },
    TtsStarted {
        text: String,
        timestamp: u64,
    },
    TtsCompleted {
        text: String,
        timestamp: u64,
    },
    Error {
        message: String,
        code: Option<i32>,
    },
    Metrics {
        key: String,
        duration: u64,
    },
    Ping {
        timestamp: u64,
    },
    Pong {
        timestamp: u64,
    },
    Connected {
        server: String,
        version: String,
    },
}

/// Pipecat event sender type
pub type PipecatEventSender = mpsc::UnboundedSender<PipecatEvent>;

/// Pipecat event receiver type  
pub type PipecatEventReceiver = mpsc::UnboundedReceiver<PipecatEvent>;

/// Create a new event channel for Pipecat events
pub fn create_event_channel() -> (PipecatEventSender, PipecatEventReceiver) {
    mpsc::unbounded_channel()
}

/// Check if Pipecat integration is enabled in configuration
pub fn is_enabled(config: &crate::config::Config) -> bool {
    config.pipecat.as_ref()
        .map(|p| p.enabled)
        .unwrap_or(false)
}

/// Check if we should use Pipecat for AI processing
pub fn should_use_pipecat(config: &crate::config::Config) -> bool {
    let enabled = is_enabled(config);
    let use_for_ai = config.pipecat.as_ref()
        .map(|p| p.use_for_ai)
        .unwrap_or(false);
    
    tracing::debug!(
        enabled = enabled,
        use_for_ai = use_for_ai,
        "Checking if Pipecat should be used for AI processing"
    );
    
    enabled && use_for_ai
}

/// Get Pipecat server URL from configuration
pub fn get_server_url(config: &crate::config::Config) -> Option<String> {
    config.pipecat.as_ref()
        .and_then(|p| p.server_url.clone())
}

/// Create a new Pipecat client from configuration
pub async fn create_client(config: &crate::config::Config) -> Result<PipecatClient> {
    let pipecat_config = config.pipecat.as_ref()
        .ok_or_else(|| anyhow!("Pipecat configuration not found"))?;
    
    PipecatClient::new(pipecat_config.clone()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn test_pipecat_config_detection() {
        let mut config = Config::default();
        assert!(!is_enabled(&config));
        assert!(!should_use_pipecat(&config));

        // Add pipecat config
        config.pipecat = Some(crate::pipecat::config::PipecatConfig {
            enabled: true,
            server_url: Some("ws://localhost:8765/ws/rustpbx".to_string()),
            use_for_ai: true,
            ..Default::default()
        });

        assert!(is_enabled(&config));
        assert!(should_use_pipecat(&config));
        assert_eq!(
            get_server_url(&config),
            Some("ws://localhost:8765/ws/rustpbx".to_string())
        );
    }

    #[test]
    fn test_pipecat_message_serialization() {
        let audio_frame = PipecatAudioFrame {
            audio_data: vec![1, 2, 3, 4],
            sample_rate: 16000,
            channels: 1,
            timestamp: 12345,
            frame_id: "test_frame".to_string(),
        };

        let message = PipecatMessage::Audio(audio_frame);
        let serialized = serde_json::to_string(&message).unwrap();
        let deserialized: PipecatMessage = serde_json::from_str(&serialized).unwrap();

        match deserialized {
            PipecatMessage::Audio(frame) => {
                assert_eq!(frame.audio_data, vec![1, 2, 3, 4]);
                assert_eq!(frame.sample_rate, 16000);
                assert_eq!(frame.channels, 1);
                assert_eq!(frame.timestamp, 12345);
                assert_eq!(frame.frame_id, "test_frame");
            }
            _ => panic!("Unexpected message type"),
        }
    }
}