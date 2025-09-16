use anyhow::{Result, anyhow};
use async_trait::async_trait;
use crate::synthesis::{SynthesisClient, SynthesisType, SynthesisOption, SynthesisEvent};
use futures::stream::BoxStream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

/// Valid Deepgram Aura voice models
fn is_valid_deepgram_model(model: &str) -> bool {
    match model {
        // Aura models (English)
        "aura-asteria-en" | "aura-luna-en" | "aura-stella-en" | "aura-athena-en" |
        "aura-hera-en" | "aura-orion-en" | "aura-arcas-en" | "aura-perseus-en" |
        "aura-angus-en" | "aura-orpheus-en" | "aura-helios-en" | "aura-zeus-en" |
        "base" => true,
        // Allow any aura model as fallback
        _ => model.starts_with("aura-")
    }
}

pub struct DeepgramTtsClient {
    option: SynthesisOption,
    event_sender: Arc<Mutex<Option<mpsc::UnboundedSender<Result<SynthesisEvent>>>>>,
}

impl DeepgramTtsClient {
    pub fn new(option: SynthesisOption) -> Self {
        Self {
            option,
            event_sender: Arc::new(Mutex::new(None)),
        }
    }

    pub fn create(option: &SynthesisOption) -> anyhow::Result<Box<dyn SynthesisClient>> {
        Ok(Box::new(Self::new(option.clone())))
    }
}

#[async_trait]
impl SynthesisClient for DeepgramTtsClient {
    fn provider(&self) -> SynthesisType {
        SynthesisType::Deepgram
    }

    async fn start(
        &self,
        _cancel_token: CancellationToken,
    ) -> Result<BoxStream<'static, Result<SynthesisEvent>>> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.event_sender.lock().await = Some(tx);

        let stream = UnboundedReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn synthesize(
        &self,
        text: &str,
        end_of_stream: Option<bool>,
        _option: Option<SynthesisOption>,
    ) -> Result<()> {
        let sender = self.event_sender.lock().await;
        let sender = match sender.as_ref() {
            Some(s) => s,
            None => return Err(anyhow!("TTS stream not started. Call start() first.")),
        };

        // Get API key from option or environment
        let api_key = self.option.secret_key.clone()
            .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok())
            .ok_or_else(|| anyhow!("No Deepgram API key provided"))?;

        if text.trim().is_empty() {
            if end_of_stream.unwrap_or(false) {
                let _ = sender.send(Ok(SynthesisEvent::Finished {
                    end_of_stream: Some(true),
                    cache_key: None,
                }));
            }
            return Ok(());
        }

        warn!("Synthesizing text with Deepgram: {}", text);

        let client = reqwest::Client::new();
        // Build URL with query parameters (model goes in URL, not body)
        let base_url = self.option.endpoint.as_deref()
            .unwrap_or("https://api.deepgram.com/v1/speak");
        
        let mut url = url::Url::parse(base_url)?;
        
        // Add model as query parameter with validation
        let model = if let Some(speaker) = &self.option.speaker {
            // Validate that the speaker is a valid Deepgram model
            if is_valid_deepgram_model(speaker) {
                speaker.clone()
            } else {
                warn!("Invalid Deepgram model '{}', using default 'aura-asteria-en'", speaker);
                "aura-asteria-en".to_string()
            }
        } else {
            "aura-asteria-en".to_string()
        };
        url.query_pairs_mut().append_pair("model", &model);
        
        // Add encoding as query parameter (map to valid Deepgram encodings)
        let encoding = match self.option.codec.as_deref() {
            Some("wav") => "linear16",     // WAV format uses linear16 encoding
            Some("pcm") => "linear16",     // PCM is also linear16
            Some("mp3") => "mp3", 
            Some("flac") => "flac",
            Some("opus") => "opus",
            Some("aac") => "aac",
            Some("mulaw") => "mulaw",
            Some("alaw") => "alaw",
            _ => "linear16", // Default to linear16 (WAV)
        };
        url.query_pairs_mut().append_pair("encoding", encoding);
        
        // Add sample rate as query parameter if specified
        if let Some(sample_rate) = self.option.samplerate {
            url.query_pairs_mut().append_pair("sample_rate", &sample_rate.to_string());
        }

        // Simple JSON body with just text (as per API spec)
        let request_body = serde_json::json!({
            "text": text
        });
        
        debug!("Sending TTS request to: {}", url);
        debug!("Request body: {}", request_body);

        let resp = client
            .post(url.as_str())
            .header(AUTHORIZATION, format!("Token {}", api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(&request_body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            let error_msg = format!("Deepgram TTS error ({}): {}", status, error_text);
            error!("{}", error_msg);
            return Err(anyhow!(error_msg));
        }

        // Stream the audio data in chunks
        let bytes = resp.bytes().await?;
        if !bytes.is_empty() {
            debug!("Received {} bytes from Deepgram TTS", bytes.len());

            // Send audio in chunks for better streaming experience
            const CHUNK_SIZE: usize = 4096;
            for chunk in bytes.chunks(CHUNK_SIZE) {
                let _ = sender.send(Ok(SynthesisEvent::AudioChunk(chunk.to_vec())));
            }
        }

        // Send end of stream if requested
        if end_of_stream.unwrap_or(false) {
            let _ = sender.send(Ok(SynthesisEvent::Finished {
                end_of_stream: Some(true),
                cache_key: None,
            }));
        }

        debug!("Deepgram TTS synthesis completed");
        Ok(())
    }
}