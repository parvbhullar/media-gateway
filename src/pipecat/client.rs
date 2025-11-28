use anyhow::{Result, anyhow};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::{Instant, sleep, timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;

use super::{
    ConnectionStatus, PipecatEvent, PipecatEventSender, PipecatMessage, PipecatResponse,
    config::PipecatConfig,
};

type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Pipecat client for WebSocket communication
pub struct PipecatClient {
    config: PipecatConfig,
    connection_status: Arc<RwLock<ConnectionStatus>>,
    websocket: Arc<Mutex<Option<WebSocket>>>,
    event_sender: PipecatEventSender,
    room_id: String,
    reconnect_attempts: Arc<Mutex<u32>>,
    last_ping_time: Arc<Mutex<Option<Instant>>>,
    audio_frame_counter: Arc<Mutex<u64>>,
}

impl PipecatClient {
    /// Create a new Pipecat client
    pub async fn new(config: PipecatConfig) -> Result<Self> {
        // Validate configuration
        config
            .validate()
            .map_err(|e| anyhow!("Invalid Pipecat config: {}", e))?;

        let (event_sender, _event_receiver) = mpsc::unbounded_channel();
        let room_id = format!("rustpbx_{}", Uuid::new_v4().simple());

        let client = Self {
            config,
            connection_status: Arc::new(RwLock::new(ConnectionStatus::Disconnected)),
            websocket: Arc::new(Mutex::new(None)),
            event_sender,
            room_id,
            reconnect_attempts: Arc::new(Mutex::new(0)),
            last_ping_time: Arc::new(Mutex::new(None)),
            audio_frame_counter: Arc::new(Mutex::new(0)),
        };

        Ok(client)
    }

    /// Create a new client with event receiver
    pub async fn with_event_receiver(
        config: PipecatConfig,
    ) -> Result<(Self, crate::pipecat::PipecatEventReceiver)> {
        config
            .validate()
            .map_err(|e| anyhow!("Invalid Pipecat config: {}", e))?;

        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let room_id = format!("rustpbx_{}", Uuid::new_v4().simple());

        let client = Self {
            config,
            connection_status: Arc::new(RwLock::new(ConnectionStatus::Disconnected)),
            websocket: Arc::new(Mutex::new(None)),
            event_sender,
            room_id,
            reconnect_attempts: Arc::new(Mutex::new(0)),
            last_ping_time: Arc::new(Mutex::new(None)),
            audio_frame_counter: Arc::new(Mutex::new(0)),
        };

        Ok((client, event_receiver))
    }

    /// Connect to the Pipecat server
    pub async fn connect(&self) -> Result<()> {
        info!(
            "Connecting to Pipecat server: {}",
            self.config.get_server_url()
        );

        *self.connection_status.write().await = ConnectionStatus::Connecting;

        let url = Url::parse(&self.config.get_server_url())
            .map_err(|e| anyhow!("Invalid Pipecat server URL: {}", e))?;

        // Connect with timeout
        let connect_future = connect_async(url.as_str());
        let (ws_stream, response) =
            timeout(self.config.connection_timeout_duration(), connect_future)
                .await
                .map_err(|_| anyhow!("Connection timeout"))?
                .map_err(|e| anyhow!("WebSocket connection failed: {}", e))?;

        info!("Connected to Pipecat server, status: {}", response.status());

        // Store the WebSocket connection
        *self.websocket.lock().await = Some(ws_stream);
        *self.connection_status.write().await = ConnectionStatus::Connected;
        *self.reconnect_attempts.lock().await = 0;

        // ✅ Send initial configuration BEFORE starting message loop
        self.send_configuration().await?;
        info!("✅ Sent initial configuration to Pipecat server");

        // ✅ Start ping task
        self.start_ping_task().await;

        // ✅ Start message loop in a separate task (non-blocking)
        let websocket = self.websocket.clone();
        let event_sender = self.event_sender.clone();
        let connection_status = self.connection_status.clone();
        let config = self.config.clone();
        
        tokio::spawn(async move {
            Self::message_loop_task(websocket, event_sender, connection_status, config).await;
        });

        Ok(())
    }

    // Add a new static method for the message loop
    async fn message_loop_task(
        websocket: Arc<Mutex<Option<WebSocket>>>,
        event_sender: PipecatEventSender,
        connection_status: Arc<RwLock<ConnectionStatus>>,
        config: PipecatConfig,
    ) {
        info!("📡 Starting Pipecat message loop");
        
        loop {
            // ✅ SHORT lock to check and read one message
            let message_result = {
                let mut ws_guard = websocket.lock().await;
                
                if let Some(ws) = ws_guard.as_mut() {
                    // Read one message while holding the lock
                    match timeout(Duration::from_millis(100), ws.next()).await {
                        Ok(Some(msg)) => Some(msg),
                        Ok(None) => {
                            // Stream ended
                            drop(ws_guard);
                            break;
                        }
                        Err(_) => {
                            // Timeout - release lock and continue
                            None
                        }
                    }
                } else {
                    // No websocket
                    drop(ws_guard);
                    warn!("No websocket in message loop");
                    break;
                }
            };
            // ✅ Lock is released here!
            
            // Process the message outside the lock
            if let Some(Ok(msg)) = message_result {
                match msg {
                    Message::Text(text) => {
                        if config.debug_logging {
                            debug!("📥 Received from Pipecat: {}", text);
                        }
                        
                        if let Ok(response) = serde_json::from_str::<PipecatResponse>(&text) {
                            if let Some(event) = Self::response_to_event(response) {
                                let _ = event_sender.send(event);
                            }
                        }
                    }
                    Message::Binary(data) => {
                        debug!("🎵 Received audio from Pipecat: {} bytes", data.len());
                        
                        let _ = event_sender.send(PipecatEvent::AudioResponse {
                            audio_data: data.to_vec(),
                            sample_rate: config.audio.sample_rate,
                            channels: config.audio.channels as u32,
                        });
                    }
                    Message::Close(_) => {
                        info!("Pipecat server closed connection");
                        break;
                    }
                    Message::Ping(data) => {
                        // Respond to ping
                        let mut ws_guard = websocket.lock().await;
                        if let Some(ws) = ws_guard.as_mut() {
                            let _ = ws.send(Message::Pong(data)).await;
                        }
                    }
                    Message::Pong(_) => {
                        debug!("Received pong from Pipecat");
                    }
                    _ => {}
                }
            } else if let Some(Err(e)) = message_result {
                error!("WebSocket error in message loop: {}", e);
                break;
            }
            
            // Small yield to prevent busy loop
            tokio::time::sleep(Duration::from_micros(100)).await;
        }
        
        // Clean up
        *websocket.lock().await = None;
        *connection_status.write().await = ConnectionStatus::Disconnected;
        info!("📡 Pipecat message loop ended");
    }

    /// Disconnect from the Pipecat server
    pub async fn disconnect(&self) -> Result<()> {
        info!("Disconnecting from Pipecat server");

        // Send disconnect message
        let disconnect_msg = PipecatMessage::Disconnect {
            reason: "Client disconnect".to_string(),
        };
        let _ = self.send_message(disconnect_msg).await;

        // Close WebSocket
        if let Some(mut ws) = self.websocket.lock().await.take() {
            let _ = ws.close(None).await;
        }

        *self.connection_status.write().await = ConnectionStatus::Disconnected;

        info!("Disconnected from Pipecat server");
        Ok(())
    }

    /// Send audio frame to Pipecat server with retry logic
    pub async fn send_audio(&self, audio_data: Vec<u8>) -> Result<()> {
        // ✅ UNCOMMENT: Check connection status
        if !self.is_connected().await {
            if self.config.fallback_to_internal {
                return Ok(()); // Silently skip, will use internal ASR
            } else {
                return Err(anyhow!("Not connected to Pipecat server"));
            }
        }

        // Check if audio data is valid
        if audio_data.is_empty() {
            return Ok(());
        }

        let audio_len = audio_data.len();

        let mut frame_counter = self.audio_frame_counter.lock().await;
        *frame_counter += 1;
        let frame_number = *frame_counter;
        drop(frame_counter); // ✅ Release lock early

        // ✅ Log every frame for debugging (remove after testing)
        if frame_number <= 10 || frame_number % 50 == 0 {
            info!("🎤 Sending audio frame #{} ({} bytes) to Pipecat", frame_number, audio_len);
        }

        // ✅ SHORT lock duration - get websocket reference quickly
        let mut ws_guard = self.websocket.lock().await;

        if let Some(ws) = ws_guard.as_mut() {
            // ✅ Option 1: Send as binary (raw PCM audio)
            let result = ws.send(Message::Binary(audio_data.into())).await;
            
            // ✅ IMPORTANT: Release lock immediately
            drop(ws_guard);
            
            match result {
                Ok(_) => {
                    if frame_number % 100 == 0 {
                        info!("✅ Successfully sent audio frame #{} to Pipecat", frame_number);
                    }
                    Ok(())
                }
                Err(e) => {
                    error!("❌ Failed to send audio frame #{} to Pipecat: {}", frame_number, e);
                    *self.connection_status.write().await = ConnectionStatus::Error(e.to_string());
                    Err(anyhow!("Failed to send audio: {}", e))
                }
            }
        } else {
            drop(ws_guard);
            warn!("⚠️ WebSocket not connected when trying to send frame #{}", frame_number);
            Err(anyhow!("WebSocket not connected"))
        }
    }

    /// Update system prompt
    pub async fn update_system_prompt(&self, prompt: String) -> Result<()> {
        let configure_msg = PipecatMessage::Configure {
            room_id: self.room_id.clone(),
            system_prompt: Some(prompt),
            stt_config: None,
            llm_config: None,
            tts_config: None,
        };

        self.send_message(configure_msg).await
    }

    /// Check if connected to Pipecat server
    pub async fn is_connected(&self) -> bool {
        matches!(
            *self.connection_status.read().await,
            ConnectionStatus::Connected
        )
    }

    /// Get current connection status
    pub async fn get_status(&self) -> ConnectionStatus {
        self.connection_status.read().await.clone()
    }

    /// Start with automatic reconnection
    pub async fn start_with_reconnect(&self) -> Result<()> {
        info!("Starting connection to Pipecat server with reconnection enabled");
        
        loop {
            match self.connect().await {
                Ok(_) => {
                    info!("✓ Successfully connected to Pipecat server at {}", self.config.get_server_url());
                    
                    // ✅ This will block until connection is lost
                    self.run_message_loop().await;
                    
                    // Connection lost - will reconnect
                    warn!("Connection lost, will reconnect in 5 seconds...");
                    *self.connection_status.write().await = ConnectionStatus::Disconnected;
                }
                Err(e) => {
                    error!("Failed to connect: {}", e);
                    *self.connection_status.write().await = ConnectionStatus::Disconnected;
                }
            }
            
            // Wait before reconnecting
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Send a message to the Pipecat server
    async fn send_message(&self, message: PipecatMessage) -> Result<()> {
        let mut ws_guard = self.websocket.lock().await;

        if let Some(ws) = ws_guard.as_mut() {
            let json_str = serde_json::to_string(&message)
                .map_err(|e| anyhow!("Failed to serialize message: {}", e))?;

            if self.config.debug_logging {
                debug!("Sending to Pipecat: {}", json_str);
            }

            ws.send(Message::Text(json_str.into()))
                .await
                .map_err(|e| anyhow!("Failed to send message: {}", e))?;

            Ok(())
        } else {
            Err(anyhow!("WebSocket not connected"))
        }
    }

    /// Send initial configuration to Pipecat server
    async fn send_configuration(&self) -> Result<()> {
        let configure_msg = PipecatMessage::Configure {
            room_id: self.room_id.clone(),
            system_prompt: self.config.default_system_prompt.clone(),
            stt_config: Some(serde_json::json!({
                "sample_rate": self.config.audio.sample_rate,
                "language": "en",
                "model": "nova"
            })),
            llm_config: Some(serde_json::json!({
                "model": "gpt-4o-mini",
                "max_tokens": 150,
                "temperature": 0.7
            })),
            tts_config: Some(serde_json::json!({
                "model": "aura-asteria-en",
                "sample_rate": self.config.audio.sample_rate,
                "encoding": self.config.audio.encoding
            })),
        };

        self.send_message(configure_msg).await
    }

    /// Start the message handling loop (blocking)
    async fn run_message_loop(&self) {
        let mut ws_guard = self.websocket.lock().await;
        let event_sender = self.event_sender.clone();
        
        if let Some(ws) = ws_guard.as_mut() {
            loop {
                match ws.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if self.config.debug_logging {
                            debug!("Received from Pipecat: {}", text);
                        }
                        
                        // Parse and handle the message
                        if let Ok(response) = serde_json::from_str::<PipecatResponse>(&text) {
                            if let Some(event) = Self::response_to_event(response) {
                                let _ = event_sender.send(event);
                            }
                        }
                    }
                    Some(Ok(Message::Binary(data))) => {
                        debug!("Received binary data: {} bytes", data.len());
                        
                        // Handle audio response
                        let _ = event_sender.send(PipecatEvent::AudioResponse {
                            audio_data: data.to_vec(),
                            sample_rate: self.config.audio.sample_rate,
                            channels: self.config.audio.channels as u32,
                        });
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("Pipecat server closed connection");
                        break;
                    }
                    Some(Err(e)) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        debug!("WebSocket stream ended");
                        break;
                    }
                    _ => {}
                }
            }
        }
        
        // Clean up
        *self.websocket.lock().await = None;
    }

    /// Start periodic ping task
    async fn start_ping_task(&self) {
        let websocket = self.websocket.clone();
        let last_ping_time = self.last_ping_time.clone();
        let connection_status = self.connection_status.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                interval.tick().await;

                // Check if still connected
                if !matches!(*connection_status.read().await, ConnectionStatus::Connected) {
                    break;
                }

                // Send ping
                let ping_msg = PipecatMessage::Ping {
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                };

                let mut ws_guard = websocket.lock().await;
                if let Some(ws) = ws_guard.as_mut() {
                    let json_str = serde_json::to_string(&ping_msg).unwrap();

                    if let Err(e) = ws.send(Message::Text(json_str.into())).await {
                        error!("Failed to send ping: {}", e);
                        *connection_status.write().await = ConnectionStatus::Error(e.to_string());
                        break;
                    }

                    *last_ping_time.lock().await = Some(Instant::now());
                } else {
                    break;
                }
            }
        });
    }

    /// Convert Pipecat response to event
    fn response_to_event(response: PipecatResponse) -> Option<PipecatEvent> {
        match response {
            PipecatResponse::Audio {
                audio_data,
                sample_rate,
                channels,
                ..
            } => Some(PipecatEvent::AudioResponse {
                audio_data,
                sample_rate,
                channels,
            }),
            PipecatResponse::Transcription {
                text,
                is_final,
                timestamp,
                ..
            } => {
                if is_final {
                    Some(PipecatEvent::TranscriptionFinal { text, timestamp })
                } else {
                    Some(PipecatEvent::TranscriptionDelta { text, timestamp })
                }
            }
            PipecatResponse::LlmResponse {
                text,
                is_complete,
                timestamp,
            } => Some(PipecatEvent::LlmResponse {
                text,
                is_complete,
                timestamp,
            }),
            PipecatResponse::TtsStarted { text, timestamp } => {
                Some(PipecatEvent::TtsStarted { text, timestamp })
            }
            PipecatResponse::TtsCompleted { text, timestamp } => {
                Some(PipecatEvent::TtsCompleted { text, timestamp })
            }
            PipecatResponse::Error { message, code, .. } => {
                Some(PipecatEvent::Error { message, code })
            }
            PipecatResponse::Metrics { key, duration, .. } => {
                Some(PipecatEvent::Metrics { key, duration })
            }
            PipecatResponse::Ping { timestamp } => Some(PipecatEvent::Ping { timestamp }),
            PipecatResponse::Pong { timestamp } => Some(PipecatEvent::Pong { timestamp }),
            PipecatResponse::Connected {
                server, version, ..
            } => Some(PipecatEvent::Connected { server, version }),
            PipecatResponse::Configured {
                call_id, status, ..
            } => {
                debug!(
                    "Pipecat configuration confirmed for call {}: {}",
                    call_id, status
                );
                None // No event needed for configuration confirmation
            }
        }
    }
}
