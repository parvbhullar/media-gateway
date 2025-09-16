use super::{TranscriptionClient, TranscriptionOption, handle_wait_for_answer_with_audio_drop};
use crate::{
    Sample, TrackId,
    event::{EventSender, SessionEvent},
    media::codecs,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn, error};
use uuid::Uuid;

struct DeepgramAsrClientInner {
    audio_tx: mpsc::UnboundedSender<Vec<u8>>,
    option: TranscriptionOption,
}

pub struct DeepgramAsrClient {
    inner: Arc<DeepgramAsrClientInner>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum DgEvent {
    #[serde(rename = "Results")]
    Results {
        is_final: Option<bool>,
        start: Option<f64>,
        duration: Option<f64>,
        speech_final: Option<bool>,
        channel: DgChannel,
        metadata: Option<DgMetadata>,
    },
    #[serde(rename = "Metadata")]
    Metadata { 
        request_id: Option<String>,
        transaction_key: Option<String>,
        sha256: Option<String>,
        created: Option<String>,
        duration: Option<f64>,
        channels: Option<u32>,
        models: Option<Vec<String>>,
    },
    #[serde(rename = "SpeechStarted")]
    SpeechStarted {
        timestamp: Option<f64>,
    },
    #[serde(rename = "UtteranceEnd")]
    UtteranceEnd {
        timestamp: Option<f64>,
    },
    #[serde(rename = "CloseStream")]
    CloseStream { 
        request_id: Option<String>,
    },
    #[serde(rename = "Error")]
    Error {
        description: String,
        message: Option<String>,
        variant: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct DgChannel {
    alternatives: Vec<DgAlternative>,
}

#[derive(Debug, Deserialize)]
struct DgAlternative {
    transcript: String,
    confidence: Option<f64>,
    words: Option<Vec<DgWord>>,
}

#[derive(Debug, Deserialize)]
struct DgWord {
    word: String,
    start: f64,
    end: f64,
    confidence: Option<f64>,
    punctuated_word: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DgMetadata {
    request_id: Option<String>,
    transaction_key: Option<String>,
    sha256: Option<String>,
    created: Option<String>,
    duration: Option<f64>,
    channels: Option<u32>,
}

#[derive(Debug, Serialize)]
struct KeepAlive {
    #[serde(rename = "type")]
    msg_type: String,
}

impl DeepgramAsrClientInner {
    async fn connect_websocket(
        &self,
        voice_id: &String,
    ) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
        let api_key = self
            .option
            .secret_key
            .as_deref()
            .ok_or_else(|| anyhow!("No DEEPGRAM_API_KEY provided"))?;
            
        debug!(voice_id, "Using API key: {}...", &api_key[..std::cmp::min(8, api_key.len())]);
        
        if api_key.trim().is_empty() {
            return Err(anyhow!("DEEPGRAM_API_KEY is empty"));
        }

        // Build query parameters with proper defaults
        let model = self.option.model_type.clone().unwrap_or_else(|| "nova".to_string());
        let sample_rate = self.option.samplerate.unwrap_or(16_000);
        let language = self.option.language.clone().unwrap_or_else(|| "en".to_string());
        
        // Validate known good models
        let valid_models = ["nova", "nova-2", "nova-2-general", "nova-2-phonecall", "nova-2-finance", "nova-2-conversationalai", 
                          "nova-2-voicemail", "nova-2-video", "nova-2-meeting", "nova-2-medical", "nova-2-drivethru"];
        if !valid_models.contains(&model.as_str()) {
            warn!(voice_id, "Unknown Deepgram model '{}', proceeding anyway", model);
        }
        
        debug!(voice_id, "Deepgram ASR Config - Model: '{}', Language: '{}', Sample Rate: {}", model, language, sample_rate);
        
        // Set transcription features (using sensible defaults since TranscriptionOption doesn't have these fields)
        let interim_results = true;
        let punctuate = true;
        let smart_format = true;
        let endpointing = 1000; // 1 second
        
        let base_url = self
            .option
            .endpoint
            .as_deref()
            .unwrap_or("wss://api.deepgram.com/v1/listen");

        let ws_url = format!(
            "{base}?model={model}&language={language}&encoding=linear16&sample_rate={sr}&channels=1&interim_results={interim}&punctuate={punctuate}&smart_format={smart}&endpointing={endpoint}&vad_events=true",
            base = base_url,
            model = urlencoding::encode(&model),
            language = urlencoding::encode(&language),
            sr = sample_rate,
            interim = interim_results,
            punctuate = punctuate,
            smart = smart_format,
            endpoint = endpointing
        );

        debug!(
            voice_id,
            "Connecting to Deepgram with language '{}' and model '{}'", language, model
        );
        debug!(voice_id, "WebSocket URL: {}", ws_url);

        let mut request = ws_url.clone().into_client_request()?;
        let headers = request.headers_mut();
        headers.insert("Authorization", format!("Token {}", api_key).parse()?);
        headers.insert("User-Agent", "DeepgramRustSDK/1.0.0".parse()?);

        debug!(voice_id, "Attempting WebSocket connection...");
        let (ws_stream, response) = match connect_async(request).await {
            Ok((stream, resp)) => {
                debug!(voice_id, "WebSocket connected successfully, status: {}", resp.status());
                (stream, resp)
            }
            Err(e) => {
                error!(voice_id, "WebSocket connection failed: {}", e);
                error!(voice_id, "Connection URL was: {}", ws_url);
                error!(voice_id, "Model: {}, Language: {}, Sample Rate: {}", model, language, sample_rate);
                return Err(anyhow!("Failed to connect to Deepgram WebSocket: {}", e));
            }
        };
        
        debug!(
            voice_id,
            "Deepgram WebSocket established. Response: {}", 
            response.status()
        );
        
        match response.status() {
            StatusCode::SWITCHING_PROTOCOLS => Ok(ws_stream),
            _ => Err(anyhow!(
                "Failed to connect to Deepgram WebSocket: {} - {:?}", 
                response.status(), 
                response
            )),
        }
    }
}

impl DeepgramAsrClient {
    async fn handle_websocket_message(
        track_id: TrackId,
        ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        mut audio_rx: mpsc::UnboundedReceiver<Vec<u8>>,
        event_sender: EventSender,
        token: CancellationToken,
    ) -> Result<()> {
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        let begin_time = crate::get_timestamp();

        // Receiver task: handle incoming messages from Deepgram
        let event_sender_clone = event_sender.clone();
        let track_id_clone = track_id.clone();
        let token_clone = token.clone();
        
        let receiver_task = tokio::spawn(async move {
            while let Some(msg) = ws_receiver.next().await {
                if token_clone.is_cancelled() {
                    break;
                }

                match msg {
                    Ok(Message::Text(text)) => {
                        debug!(track_id_clone, "Received: {}", text);
                        
                        match serde_json::from_str::<DgEvent>(&text) {
                            Ok(DgEvent::Results { 
                                is_final, 
                                start, 
                                duration, 
                                channel, 
                                speech_final,
                                .. 
                            }) => {
                                if channel.alternatives.is_empty() { 
                                    continue; 
                                }
                                
                                let alternative = &channel.alternatives[0];
                                let transcript = &alternative.transcript;
                                
                                if transcript.trim().is_empty() { 
                                    continue; 
                                }

                                // Calculate timing
                                let s_start = start.unwrap_or(0.0);
                                let s_dur = duration.unwrap_or(0.0);
                                let sentence_start_time = begin_time + (s_start * 1000.0) as u64;
                                let sentence_end_time = sentence_start_time + (s_dur * 1000.0) as u64;

                                let is_final_result = is_final.unwrap_or(false) || speech_final.unwrap_or(false);

                                let event = if is_final_result {
                                    SessionEvent::AsrFinal {
                                        track_id: track_id_clone.clone(),
                                        index: 0,
                                        text: transcript.clone(),
                                        timestamp: crate::get_timestamp(),
                                        start_time: Some(sentence_start_time),
                                        end_time: Some(sentence_end_time),
                                    }
                                } else {
                                    SessionEvent::AsrDelta {
                                        track_id: track_id_clone.clone(),
                                        index: 0,
                                        text: transcript.clone(),
                                        timestamp: crate::get_timestamp(),
                                        start_time: Some(sentence_start_time),
                                        end_time: Some(sentence_end_time),
                                    }
                                };
                                
                                let _ = event_sender_clone.send(event);

                                // Send metrics
                                let diff_time = (crate::get_timestamp() - begin_time) as u32;
                                let key = if is_final_result { 
                                    "completed.asr.deepgram" 
                                } else { 
                                    "ttfb.asr.deepgram" 
                                };
                                
                                let _ = event_sender_clone.send(SessionEvent::Metrics {
                                    timestamp: crate::get_timestamp(),
                                    key: key.to_string(),
                                    data: serde_json::json!({
                                        "confidence": alternative.confidence,
                                        "words_count": alternative.words.as_ref().map(|w| w.len()).unwrap_or(0)
                                    }),
                                    duration: diff_time,
                                });
                            }
                            Ok(DgEvent::Metadata { .. }) => {
                                debug!(track_id_clone, "Received metadata from Deepgram");
                            }
                            Ok(DgEvent::SpeechStarted { timestamp }) => {
                                debug!(track_id_clone, "Speech started at: {:?}", timestamp);
                            }
                            Ok(DgEvent::UtteranceEnd { timestamp }) => {
                                debug!(track_id_clone, "Utterance ended at: {:?}", timestamp);
                            }
                            Ok(DgEvent::CloseStream { .. }) => {
                                info!(track_id_clone, "Deepgram closed the stream");
                                break;
                            }
                            Ok(DgEvent::Error { description, message, variant }) => {
                                error!(
                                    track_id_clone, 
                                    "Deepgram error: {} - {:?} (variant: {:?})", 
                                    description, message, variant
                                );
                                let _ = event_sender_clone.send(SessionEvent::Error {
                                    timestamp: crate::get_timestamp(),
                                    track_id: track_id_clone.clone(),
                                    sender: "DeepgramAsrClient".to_string(),
                                    error: format!("Deepgram error: {}", description),
                                    code: Some(400),
                                });
                                break;
                            }
                            Ok(DgEvent::Other) => {
                                debug!(track_id_clone, "Received unknown event type");
                            }
                            Err(e) => {
                                warn!(track_id_clone, "Failed to parse Deepgram message: {} - Raw: {}", e, text);
                            }
                        }
                    }
                        Ok(Message::Close(close_frame)) => {
                            info!(track_id_clone, "WebSocket closed by server: {:?}", close_frame);
                            break;
                        }
                        Ok(Message::Ping(_)) => {
                            debug!(track_id_clone, "Received ping from server");
                        }
                        Ok(Message::Pong(_)) => {
                            debug!(track_id_clone, "Received pong from server");
                        }
                        Ok(Message::Binary(_)) => {
                            warn!(track_id_clone, "Received unexpected binary message");
                        }
                        Ok(Message::Frame(_)) => {
                            // New variant in tungstenite 0.27 – usually internal; safe to ignore
                            debug!(track_id_clone, "Received Frame variant (ignored)");
                        }
                        Err(e) => {
                            warn!(track_id_clone, "WebSocket error: {}", e);
                            return Err(anyhow!("WebSocket error: {}", e));
                    }
                }
            }
            Ok(())
        });

        // Sender task: send audio data and handle keep-alive
        let token_clone = token.clone();
        let track_id_clone = track_id.clone();
        
        // let sender_task = tokio::spawn(async move {
        let sender_task: tokio::task::JoinHandle<Result<(), anyhow::Error>> = tokio::spawn(async move {
            let mut keep_alive_interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            
            loop {
                tokio::select! {
                    // Handle incoming audio data
                    audio_data = audio_rx.recv() => {
                        match audio_data {
                            Some(data) => {
                                if token_clone.is_cancelled() { 
                                    break; 
                                }
                                
                                if let Err(e) = ws_sender.send(Message::Binary(data.into())).await {
                                    warn!(track_id_clone, "Failed to send audio: {}", e);
                                    break;
                                }
                            }
                            None => {
                                debug!(track_id_clone, "Audio channel closed, finalizing stream");
                                break;
                            }
                        }
                    }
                    // Send keep-alive messages
                    _ = keep_alive_interval.tick() => {
                        if token_clone.is_cancelled() { 
                            break; 
                        }
                        
                        let keep_alive = KeepAlive {
                            msg_type: "KeepAlive".to_string(),
                        };
                        
                        if let Ok(keep_alive_json) = serde_json::to_string(&keep_alive) {
                            if let Err(e) = ws_sender.send(Message::Text(keep_alive_json.into())).await {
                                warn!(track_id_clone, "Failed to send keep-alive: {}", e);
                                break;
                            }
                        }
                    }
                    // Handle cancellation
                    _ = token_clone.cancelled() => {
                        debug!(track_id_clone, "Sender task cancelled");
                        break;
                    }
                }
            }

            // Send finalization message
            let finalize_msg = serde_json::json!({"type": "Finalize"});
            if let Err(e) = ws_sender.send(Message::Text(finalize_msg.to_string().into())).await {
                warn!(track_id_clone, "Failed to send Finalize: {}", e);
            }
            
            // Close the websocket gracefully
            let _ = ws_sender.close().await;
            
            Ok(())
        });

        // Wait for either task to complete
        tokio::select! {
            result = receiver_task => {
                match result {
                    Ok(Ok(())) => debug!(track_id, "Receiver task completed successfully"),
                    Ok(Err(e)) => warn!(track_id, "Receiver task error: {}", e),
                    Err(e) => warn!(track_id, "Receiver task panicked: {}", e),
                }
            }
            result = sender_task => {
                match result {
                    Ok(Ok(())) => debug!(track_id, "Sender task completed successfully"),
                    Ok(Err(e)) => warn!(track_id, "Sender task error: {}", e),
                    Err(e) => warn!(track_id, "Sender task panicked: {}", e),
                }
            }
            _ = token.cancelled() => {
                debug!(track_id, "WebSocket handling cancelled");
            }
        }

        Ok(())
    }
}

pub struct DeepgramAsrClientBuilder {
    option: TranscriptionOption,
    track_id: Option<String>,
    token: Option<CancellationToken>,
    event_sender: EventSender,
}

impl DeepgramAsrClientBuilder {
    pub fn create(
        track_id: TrackId,
        token: CancellationToken,
        option: TranscriptionOption,
        event_sender: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn TranscriptionClient>>> + Send>> {
        Box::pin(async move {
            let builder = Self::new(option, event_sender);
            builder
                .with_cancel_token(token)
                .with_track_id(track_id)
                .build()
                .await
                .map(|client| Box::new(client) as Box<dyn TranscriptionClient>)
        })
    }

    pub fn new(option: TranscriptionOption, event_sender: EventSender) -> Self {
        Self { 
            option, 
            token: None, 
            track_id: None, 
            event_sender 
        }
    }

    pub fn with_cancel_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.token = Some(cancellation_token);
        self
    }

    pub fn with_secret_key(mut self, secret_key: String) -> Self {
        self.option.secret_key = Some(secret_key);
        self
    }

    pub fn with_model_type(mut self, model_type: String) -> Self {
        self.option.model_type = Some(model_type);
        self
    }

    pub fn with_track_id(mut self, track_id: String) -> Self {
        self.track_id = Some(track_id);
        self
    }

    pub async fn build(self) -> Result<DeepgramAsrClient> {
        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel();

        let event_sender_rx = match self.option.start_when_answer {
            Some(true) => Some(self.event_sender.subscribe()),
            _ => None,
        };

        let inner = Arc::new(DeepgramAsrClientInner {
            audio_tx,
            option: self.option,
        });

        let client = DeepgramAsrClient { inner: inner.clone() };
        let track_id = self.track_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let token = self.token.unwrap_or(CancellationToken::new());
        let event_sender = self.event_sender;

        info!(%track_id, "Starting Deepgram ASR client");

        tokio::spawn(async move {
            // Handle waiting for answer if configured
            if event_sender_rx.is_some() {
                handle_wait_for_answer_with_audio_drop(event_sender_rx, &mut audio_rx, &token).await;
                if token.is_cancelled() {
                    debug!("Cancelled during wait for answer");
                    return Ok::<(), anyhow::Error>(());
                }
            }

            // Connect to Deepgram WebSocket
            let ws_stream = match inner.connect_websocket(&track_id).await {
                Ok(stream) => stream,
                Err(e) => {
                    error!(track_id, "Failed to connect to Deepgram WebSocket: {}", e);
                    let _ = event_sender.send(SessionEvent::Error {
                        timestamp: crate::get_timestamp(),
                        track_id: track_id.clone(),
                        sender: "DeepgramAsrClient".to_string(),
                        error: format!("Failed to connect to Deepgram WebSocket: {}", e),
                        code: Some(500),
                    });
                    return Err(e);
                }
            };

            // Handle the WebSocket communication
            match DeepgramAsrClient::handle_websocket_message(
                track_id.clone(), 
                ws_stream, 
                audio_rx,
                event_sender.clone(), 
                token
            ).await {
                Ok(_) => {
                    debug!(track_id, "Deepgram WebSocket handling completed successfully");
                }
                Err(e) => {
                    error!(track_id, "Error in Deepgram WebSocket handling: {}", e);
                    let _ = event_sender.send(SessionEvent::Error {
                        track_id,
                        timestamp: crate::get_timestamp(),
                        sender: "deepgram_asr".to_string(),
                        error: e.to_string(),
                        code: None,
                    });
                }
            }
            
            Ok::<(), anyhow::Error>(())
        });

        Ok(client)
    }
}

#[async_trait]
impl TranscriptionClient for DeepgramAsrClient {
    fn send_audio(&self, samples: &[Sample]) -> Result<()> {
        // Convert samples to bytes using your existing utility
        let audio_data = codecs::samples_to_bytes(samples);
        self.inner
            .audio_tx
            .send(audio_data)
            .map_err(|_| anyhow!("Failed to send audio data"))?;
        Ok(())
    }
}