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
use serde::{Deserialize};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
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
        is_final: bool,
        start: Option<f64>,
        duration: Option<f64>,
        speech_final: Option<bool>,
        channel: DgChannel,
        // metadata, etc. are ignored
    },
    #[serde(rename = "Metadata")]
    Metadata { /* ignore for now */ },
    #[serde(rename = "CloseStream")]
    CloseStream { /* ignore */ },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct DgChannel {
    alternatives: Vec<DgAlt>,
}

#[derive(Debug, Deserialize)]
struct DgAlt {
    transcript: String,
    // words, confidence, etc. omitted
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

        // Build query params to mimic your Aliyun options
        let model = self.option.model_type.clone().unwrap_or_else(|| "nova-2-general".into());
        let sample_rate = self.option.samplerate.unwrap_or(16_000);
        let language = self.option.language.clone().unwrap_or_else(|| "en".into());
        // Deepgram expects encoding if sample_rate is passed explicitly
        // let interim = self.option.return_interim.unwrap_or(true);
        // let punctuate = self.option.punctuate.unwrap_or(true);
        // let smart_format = self.option.smart_format.unwrap_or(true);
        // let endpointing_ms = self.option.endpointing_ms.unwrap_or(10_00); // 1000ms default-ish
        let interim = true;
        let punctuate = true;
        let smart_format = true;
        let endpointing_ms = 1000;
        let base = self
            .option
            .endpoint
            .as_deref()
            .unwrap_or("wss://api.deepgram.com/v1/listen");

        let ws_url = format!(
            "{base}?model={model}&language={language}&encoding=linear16&sample_rate={sr}&channels=1&interim_results={interim}&punctuate={punctuate}&smart_format={smart}&endpointing={endpoint}",
            base = base,
            model = urlencoding::encode(&model),
            language = urlencoding::encode(&language),
            sr = sample_rate,
            interim = interim,
            punctuate = punctuate,
            smart = smart_format,
            endpoint = endpointing_ms
        );

        let mut request = ws_url.into_client_request()?;
        let headers = request.headers_mut();
        // Deepgram accepts "token <KEY>" or "Bearer <JWT>"
        headers.insert("Authorization", format!("token {}", api_key).parse()?);

        let (ws_stream, response) = connect_async(request).await?;
        debug!(
            voice_id,
            "Deepgram WebSocket established. Response: {}", response.status()
        );
        match response.status() {
            StatusCode::SWITCHING_PROTOCOLS => Ok(ws_stream),
            _ => Err(anyhow!("Failed to connect to Deepgram WS: {:?}", response)),
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

        // Receiver loop: parse JSON messages -> SessionEvent
        let recv_loop = async {
            while let Some(msg) = ws_receiver.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<DgEvent>(&text) {
                            Ok(DgEvent::Results { is_final, start, duration, channel, speech_final }) => {
                                if channel.alternatives.is_empty() { continue; }
                                let transcript = &channel.alternatives[0].transcript;
                                if transcript.trim().is_empty() { continue; }

                                // Deepgram times are relative to stream in seconds; convert to our u64 ms-ish timestamp scheme
                                let s_start = start.unwrap_or(0.0);
                                let s_dur = duration.unwrap_or(0.0);
                                let sentence_start_time = begin_time + (s_start * 1000.0) as u64;
                                let sentence_end_time = sentence_start_time + (s_dur * 1000.0) as u64;

                                let event = if is_final || speech_final.unwrap_or(false) {
                                    SessionEvent::AsrFinal {
                                        track_id: track_id.clone(),
                                        index: 0, // Deepgram does not supply sentence_id; keep 0 or maintain your own counter
                                        text: transcript.clone(),
                                        timestamp: crate::get_timestamp(),
                                        start_time: Some(sentence_start_time),
                                        end_time: Some(sentence_end_time),
                                    }
                                } else {
                                    SessionEvent::AsrDelta {
                                        track_id: track_id.clone(),
                                        index: 0,
                                        text: transcript.clone(),
                                        timestamp: crate::get_timestamp(),
                                        start_time: Some(sentence_start_time),
                                        end_time: Some(sentence_end_time),
                                    }
                                };
                                event_sender.send(event).ok();

                                let diff_time = (crate::get_timestamp() - begin_time) as u32;
                                let key = if is_final { "completed.asr.deepgram" } else { "ttfb.asr.deepgram" };
                                event_sender.send(SessionEvent::Metrics{
                                    timestamp: crate::get_timestamp(),
                                    key: key.to_string(),
                                    data: serde_json::json!({}),
                                    duration: diff_time,
                                }).ok();
                            }
                            Ok(DgEvent::Metadata { .. }) => { /* ignore */ }
                            Ok(DgEvent::CloseStream { .. }) => {
                                info!(track_id, "Deepgram closed stream");
                                break;
                            }
                            Ok(DgEvent::Other) => { /* ignore */ }
                            Err(e) => {
                                warn!(track_id, "Failed to parse Deepgram message: {}", e);
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        info!(track_id, "WebSocket closed by server");
                        break;
                    }
                    Err(e) => {
                        warn!(track_id, "WebSocket error: {}", e);
                        return Err(anyhow!("WebSocket error: {}", e));
                    }
                    _ => { /* ping/pong/binary not expected here */ }
                }
            }
            Ok(())
        };

        // Sender loop: forward audio frames; finalize after channel closes
        let token_clone = token.clone();
        let send_loop = async move {
            while let Some(audio_data) = audio_rx.recv().await {
                if token_clone.is_cancelled() { break; }
                // if let Err(e) = ws_sender.send(Message::Binary(audio_data)).await {
                if let Err(e) = ws_sender.send(Message::Binary(audio_data.into())).await {
                    warn!("Failed to send audio: {}", e);
                    break;
                }
            }

            // Ask Deepgram to flush & finalize remaining audio
            // Spec: send a control message {"type":"Finalize"} (text frame)
            if let Err(e) = ws_sender.send(Message::Text(r#"{"type":"Finalize"}"#.into())).await {
                warn!("Failed to send Finalize: {}", e);
            }
            Ok(())
        };

        tokio::select! {
            r = recv_loop => r,
            r = send_loop => r,
            _ = token.cancelled() => Ok(())
        }
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
        Self { option, token: None, track_id: None, event_sender }
    }

    pub fn with_cancel_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.token = Some(cancellation_token); self
    }
    pub fn with_secret_key(mut self, secret_key: String) -> Self {
        self.option.secret_key = Some(secret_key); self
    }
    pub fn with_model_type(mut self, model_type: String) -> Self {
        self.option.model_type = Some(model_type); self
    }
    pub fn with_track_id(mut self, track_id: String) -> Self {
        self.track_id = Some(track_id); self
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
            if event_sender_rx.is_some() {
                handle_wait_for_answer_with_audio_drop(event_sender_rx, &mut audio_rx, &token).await;
                if token.is_cancelled() {
                    debug!("Cancelled during wait for answer");
                    return Ok::<(), anyhow::Error>(());
                }
            }

            let ws_stream = match inner.connect_websocket(&track_id).await {
                Ok(stream) => stream,
                Err(e) => {
                    warn!(track_id, "Failed to connect to Deepgram WS: {}", e);
                    let _ = event_sender.send(SessionEvent::Error {
                        timestamp: crate::get_timestamp(),
                        track_id: track_id,
                        sender: "DeepgramAsrClient".to_string(),
                        error: format!("Failed to connect to Deepgram WebSocket: {}", e),
                        code: Some(500),
                    });
                    return Err(e);
                }
            };

            match DeepgramAsrClient::handle_websocket_message(
                track_id.clone(), ws_stream, audio_rx,
                event_sender.clone(), token
            ).await {
                Ok(_) => { debug!("Deepgram WS handling completed"); }
                Err(e) => {
                    info!("Error in Deepgram handle_websocket_message: {}", e);
                    event_sender.send(SessionEvent::Error {
                        track_id, timestamp: crate::get_timestamp(),
                        sender: "deepgram_asr".to_string(), error: e.to_string(), code: None
                    }).ok();
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
        // Your existing util converts &[Sample] to little-endian i16 PCM bytes
        let audio_data = codecs::samples_to_bytes(samples);
        self.inner
            .audio_tx
            .send(audio_data)
            .map_err(|_| anyhow!("Failed to send audio data"))?;
        Ok(())
    }
}
