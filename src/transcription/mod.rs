use crate::AudioFrame;
use crate::Sample;
use crate::event::SessionEvent;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

mod aliyun;
mod tencent_cloud;
mod voiceapi;
mod deepgram; //deepgram

pub use aliyun::AliyunAsrClient;
pub use aliyun::AliyunAsrClientBuilder;
pub use tencent_cloud::TencentCloudAsrClient;
pub use tencent_cloud::TencentCloudAsrClientBuilder;
pub use voiceapi::VoiceApiAsrClient;
pub use voiceapi::VoiceApiAsrClientBuilder;

pub use deepgram::DeepgramAsrClient;
pub use deepgram::DeepgramAsrClientBuilder;

/// Common helper function for handling wait_for_answer logic with audio dropping
pub async fn handle_wait_for_answer_with_audio_drop(
    event_rx: Option<crate::event::EventReceiver>,
    audio_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    token: &CancellationToken,
) {
    tokio::select! {
        _ = token.cancelled() => {
            debug!("Cancelled before answer");
            return;
        }
        // drop audio if not started after answer
        _ = async {
            while let Some(_) = audio_rx.recv().await {}
        } => {}
        _ = async {
            match event_rx {
                Some(mut rx) => {
                    while let Ok(event) = rx.recv().await {
                        match event {
                            SessionEvent::Answer { .. } => {
                                debug!("Received answer event, starting transcription");
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                None => {}
            }
        } => {
            debug!("Wait for answer completed");
        }
    }
}

#[derive(Debug, Clone, Serialize, Hash, Eq, PartialEq)]
pub enum TranscriptionType {
    #[serde(rename = "tencent")]
    TencentCloud,
    #[serde(rename = "voiceapi")]
    VoiceApi,
    #[serde(rename = "aliyun")]
    Aliyun,
    #[serde(rename = "deepgram")]
    Deepgram,
    Other(String),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct TranscriptionOption {
    pub provider: Option<TranscriptionType>,
    pub language: Option<String>,
    pub app_id: Option<String>,
    pub secret_id: Option<String>,
    pub secret_key: Option<String>,
    pub model_type: Option<String>,
    pub buffer_size: Option<usize>,
    pub samplerate: Option<u32>,
    pub endpoint: Option<String>,
    pub extra: Option<HashMap<String, String>>,
    pub start_when_answer: Option<bool>,
}

impl std::fmt::Display for TranscriptionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscriptionType::TencentCloud => write!(f, "tencent"),
            TranscriptionType::VoiceApi => write!(f, "voiceapi"),
            TranscriptionType::Aliyun => write!(f, "aliyun"),
            TranscriptionType::Deepgram => write!(f, "deepgram"),
            TranscriptionType::Other(provider) => write!(f, "{}", provider),
        }
    }
}

impl<'de> Deserialize<'de> for TranscriptionType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "tencent" => Ok(TranscriptionType::TencentCloud),
            "voiceapi" => Ok(TranscriptionType::VoiceApi),
            "aliyun" => Ok(TranscriptionType::Aliyun),
            "deepgram" => Ok(TranscriptionType::Deepgram),
            _ => Ok(TranscriptionType::Other(value)),
        }
    }
}

// Default config for backward compatibility
impl Default for TranscriptionOption {
    fn default() -> Self {
        Self {
            provider: None,
            language: None,
            app_id: None,
            secret_id: None,
            secret_key: None,
            model_type: None,
            buffer_size: None,
            samplerate: None,
            endpoint: None,
            extra: None,
            start_when_answer: None,
        }
    }
}

impl TranscriptionOption {
    pub fn check_default(&mut self) -> &Self {
        match self.provider {
            Some(TranscriptionType::TencentCloud) => {
                if self.app_id.is_none() {
                    self.app_id = std::env::var("TENCENT_APPID").ok();
                }
                if self.secret_id.is_none() {
                    self.secret_id = std::env::var("TENCENT_SECRET_ID").ok();
                }
                if self.secret_key.is_none() {
                    self.secret_key = std::env::var("TENCENT_SECRET_KEY").ok();
                }
            }
            Some(TranscriptionType::VoiceApi) => {
                // Set the host from environment variable if not already set
                if self.endpoint.is_none() {
                    self.endpoint = std::env::var("VOICEAPI_ENDPOINT").ok();
                }
            }
            Some(TranscriptionType::Aliyun) => {
                if self.secret_key.is_none() {
                    self.secret_key = std::env::var("DASHSCOPE_API_KEY").ok();
                }
            }
            //deegram API KEY
             Some(TranscriptionType::Deepgram) => {
                // Deepgram: key + optional endpoint override
                if self.secret_key.is_none() {
                    self.secret_key = std::env::var("DEEPGRAM_API_KEY").ok();
                }
                if self.endpoint.is_none() {
                    // Use default if not provided; can be overridden by env var if you want
                    // e.g., std::env::var("DEEPGRAM_ENDPOINT").ok()
                    self.endpoint = Some("wss://api.deepgram.com/v1/listen".to_string());
                    // self.endpoint = td::env::var("DEEPGRAM_ENDPOINT").ok();

                }
            }

            _ => {}
        }
        self
    }
}
pub type TranscriptionSender = mpsc::UnboundedSender<AudioFrame>;
pub type TranscriptionReceiver = mpsc::UnboundedReceiver<AudioFrame>;

// Unified transcription client trait with async_trait support
#[async_trait]
pub trait TranscriptionClient: Send + Sync {
    fn send_audio(&self, samples: &[Sample]) -> Result<()>;
}

#[cfg(test)]
mod tests;
