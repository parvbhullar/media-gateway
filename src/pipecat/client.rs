/*!
 * Pipecat Client for RustPBX
 * 
 * Handles WebSocket communication with the Pipecat media server
 */

use anyhow::{Result, anyhow};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{sleep, timeout, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream, MaybeTlsStream};
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};
use url::Url;
use uuid::Uuid;

use super::{
    config::PipecatConfig,
    ConnectionStatus, PipecatEvent, PipecatEventSender,
    PipecatMessage, PipecatResponse,
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
        config.validate().map_err(|e| anyhow!("Invalid Pipecat config: {}", e))?;
        
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
        config: PipecatConfig
    ) -> Result<(Self, crate::pipecat::PipecatEventReceiver)> {
        config.validate().map_err(|e| anyhow!("Invalid Pipecat config: {}", e))?;
        
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
        info!("Connecting to Pipecat server: {}", self.config.get_server_url());
        
        *self.connection_status.write().await = ConnectionStatus::Connecting;
        
        let url = Url::parse(&self.config.get_server_url())
            .map_err(|e| anyhow!("Invalid Pipecat server URL: {}", e))?;
        
        // Connect with timeout
        let connect_future = connect_async(url.as_str());
        let (ws_stream, response) = timeout(
            self.config.connection_timeout_duration(),
            connect_future
        ).await
        .map_err(|_| anyhow!("Connection timeout"))?
        .map_err(|e| anyhow!("WebSocket connection failed: {}", e))?;
        
        info!("Connected to Pipecat server, status: {}", response.status());
        
        // Store the WebSocket connection
        *self.websocket.lock().await = Some(ws_stream);
        *self.connection_status.write().await = ConnectionStatus::Connected;
        *self.reconnect_attempts.lock().await = 0;
        
        // Start message handling
        self.start_message_handler().await;
        
        // Send initial configuration
        self.send_configuration().await?;
        
        // Start ping task
        self.start_ping_task().await;
        
        Ok(())
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
        if !self.is_connected().await {
            if self.config.fallback_to_internal {
                debug!("Pipecat not connected, audio will be processed internally");
                return Ok(());
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

        // Send as raw binary data for efficiency instead of JSON
        // The server expects raw audio bytes
        let mut ws_guard = self.websocket.lock().await;

        if let Some(ws) = ws_guard.as_mut() {
            // Send raw audio bytes directly via WebSocket binary message
            match ws.send(Message::Binary(audio_data.into())).await {
                Ok(_) => {
                    if frame_number % 100 == 0 {
                        debug!("Successfully sent audio frame #{} to Pipecat ({} bytes)", frame_number, audio_len);
                    }
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to send audio to Pipecat: {}", e);
                    *self.connection_status.write().await = ConnectionStatus::Error(e.to_string());
                    Err(anyhow!("Failed to send audio: {}", e))
                }
            }
        } else {
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
        matches!(*self.connection_status.read().await, ConnectionStatus::Connected)
    }
    
    /// Get current connection status
    pub async fn get_status(&self) -> ConnectionStatus {
        self.connection_status.read().await.clone()
    }
    
    /// Start with automatic reconnection
    pub async fn start_with_reconnect(&self) -> Result<()> {
        // Check if already connected
        if self.is_connected().await {
            debug!("Already connected to Pipecat server");
            return Ok(());
        }

        if !self.config.is_reconnect_enabled() {
            info!("Attempting to connect to Pipecat server (reconnect disabled)");
            return self.connect().await;
        }

        info!("Starting connection to Pipecat server with reconnection enabled");

        loop {
            match self.connect().await {
                Ok(()) => {
                    info!("✓ Successfully connected to Pipecat server at {}", self.config.get_server_url());
                    break;
                }
                Err(e) => {
                    let mut attempts = self.reconnect_attempts.lock().await;
                    *attempts += 1;

                    if *attempts > self.config.reconnect.max_attempts {
                        error!("✗ Max reconnection attempts ({}) reached, giving up", self.config.reconnect.max_attempts);
                        *self.connection_status.write().await = ConnectionStatus::Error(
                            format!("Max reconnection attempts reached: {}", e)
                        );
                        return Err(anyhow!("Failed to connect after {} attempts: {}", *attempts, e));
                    }

                    let delay = self.config.calculate_reconnect_delay(*attempts);
                    warn!(
                        "⚠ Connection failed (attempt {}/{}): {}. Retrying in {:?}",
                        *attempts, self.config.reconnect.max_attempts, e, delay
                    );

                    sleep(delay).await;
                }
            }
        }

        Ok(())
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
            
            ws.send(Message::Text(json_str.into())).await
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
    
    /// Start the message handling loop
    async fn start_message_handler(&self) {
        let websocket = self.websocket.clone();
        let event_sender = self.event_sender.clone();
        let debug_logging = self.config.debug_logging;
        let connection_status = self.connection_status.clone();
        
        tokio::spawn(async move {
            loop {
                let message = {
                    let mut ws_guard = websocket.lock().await;
                    if let Some(ws) = ws_guard.as_mut() {
                        ws.next().await
                    } else {
                        break;
                    }
                };
                
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if debug_logging {
                            debug!("Received from Pipecat: {}", text);
                        }
                        
                        match serde_json::from_str::<PipecatResponse>(&text) {
                            Ok(response) => {
                                let event = Self::response_to_event(response);
                                if let Some(event) = event {
                                    if let Err(e) = event_sender.send(event) {
                                        error!("Failed to send Pipecat event: {}", e);
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse Pipecat response: {}. Message: {}", e, text);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("Pipecat server closed connection");
                        *connection_status.write().await = ConnectionStatus::Disconnected;
                        break;
                    }
                    Some(Err(e)) => {
                        error!("Pipecat WebSocket error: {}", e);
                        *connection_status.write().await = ConnectionStatus::Error(e.to_string());
                        break;
                    }
                    None => {
                        debug!("Pipecat WebSocket stream ended");
                        break;
                    }
                    _ => {
                        // Ignore other message types (binary, ping, pong)
                    }
                }
            }
            
            // Clean up WebSocket connection
            *websocket.lock().await = None;
        });
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
            PipecatResponse::Audio { audio_data, sample_rate, channels, .. } => {
                Some(PipecatEvent::AudioResponse {
                    audio_data,
                    sample_rate,
                    channels,
                })
            }
            PipecatResponse::Transcription { text, is_final, timestamp, .. } => {
                if is_final {
                    Some(PipecatEvent::TranscriptionFinal { text, timestamp })
                } else {
                    Some(PipecatEvent::TranscriptionDelta { text, timestamp })
                }
            }
            PipecatResponse::LlmResponse { text, is_complete, timestamp } => {
                Some(PipecatEvent::LlmResponse {
                    text,
                    is_complete,
                    timestamp,
                })
            }
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
            PipecatResponse::Ping { timestamp } => {
                Some(PipecatEvent::Ping { timestamp })
            }
            PipecatResponse::Pong { timestamp } => {
                Some(PipecatEvent::Pong { timestamp })
            }
            PipecatResponse::Connected { server, version, .. } => {
                Some(PipecatEvent::Connected { server, version })
            }
            PipecatResponse::Configured { call_id, status, .. } => {
                debug!("Pipecat configuration confirmed for call {}: {}", call_id, status);
                None // No event needed for configuration confirmation
            }
        }
    }
}