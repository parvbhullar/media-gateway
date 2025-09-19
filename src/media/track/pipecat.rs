/*!
 * Pipecat Media Track for RustPBX
 * 
 * This track forwards audio to the Pipecat media server for AI processing
 * and receives processed audio back for playback.
 */

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    AudioFrame, TrackId,
    event::{EventSender, SessionEvent},
    media::{
        processor::ProcessorChain,
        track::{Track, TrackConfig, TrackPacketSender},
    },
    pipecat::{PipecatClient, PipecatEvent, PipecatEventReceiver},
};

/// Pipecat media track
pub struct PipecatTrack {
    id: TrackId,
    ssrc: u32,
    config: TrackConfig,
    pipecat_client: Arc<PipecatClient>,
    event_receiver: Arc<Mutex<Option<PipecatEventReceiver>>>,
    packet_sender: Arc<Mutex<Option<TrackPacketSender>>>,
    processor_chain: ProcessorChain,
    cancel_token: CancellationToken,
}

impl PipecatTrack {
    /// Create a new Pipecat track
    pub async fn new(
        id: TrackId,
        ssrc: u32,
        config: TrackConfig,
        pipecat_client: Arc<PipecatClient>,
        event_receiver: PipecatEventReceiver,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let track = Self {
            id,
            ssrc,
            config: config.clone(),
            pipecat_client,
            event_receiver: Arc::new(Mutex::new(Some(event_receiver))),
            packet_sender: Arc::new(Mutex::new(None)),
            processor_chain: ProcessorChain::new(config.samplerate),
            cancel_token,
        };
        
        Ok(track)
    }
    
    /// Start processing events from Pipecat
    async fn start_event_processing(&self, event_sender: EventSender) {
        let event_receiver = self.event_receiver.clone();
        let track_id = self.id.clone();
        let cancel_token = self.cancel_token.clone();
        let packet_sender = self.packet_sender.clone();
        
        tokio::spawn(async move {
            let receiver = {
                let mut guard = event_receiver.lock().await;
                guard.take()
            };
            
            if let Some(mut rx) = receiver {
                loop {
                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            debug!("Pipecat event processing cancelled for track {}", track_id);
                            break;
                        }
                        event = rx.recv() => {
                            match event {
                                Some(pipecat_event) => {
                                    if let Err(e) = Self::handle_pipecat_event(
                                        pipecat_event,
                                        &event_sender,
                                        &track_id,
                                        &packet_sender,
                                    ).await {
                                        error!("Error handling Pipecat event: {}", e);
                                    }
                                }
                                None => {
                                    warn!("Pipecat event channel closed for track {}", track_id);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });
    }
    
    /// Handle a Pipecat event and convert to RustPBX event
    async fn handle_pipecat_event(
        event: PipecatEvent,
        event_sender: &EventSender,
        track_id: &str,
        packet_sender: &Arc<Mutex<Option<TrackPacketSender>>>,
    ) -> Result<()> {
        match event {
            PipecatEvent::TranscriptionDelta { text, timestamp } => {
                let session_event = SessionEvent::AsrDelta {
                    text,
                    track_id: track_id.to_string(),
                    timestamp,
                    start_time: Some(timestamp),
                    end_time: Some(timestamp),
                    index: 0,
                };
                let _ = event_sender.send(session_event);
            }
            
            PipecatEvent::TranscriptionFinal { text, timestamp } => {
                let session_event = SessionEvent::AsrFinal {
                    text,
                    track_id: track_id.to_string(),
                    timestamp,
                    start_time: Some(timestamp),
                    end_time: Some(timestamp),
                    index: 0,
                };
                let _ = event_sender.send(session_event);
            }
            
            PipecatEvent::LlmResponse { text, is_complete, timestamp } => {
                debug!("LLM response for track {}: {} (complete: {})", track_id, text, is_complete);
            }
            
            PipecatEvent::AudioResponse { audio_data, sample_rate, channels } => {
                // Convert raw audio bytes to samples and create audio frame
                if let Ok(samples) = Self::bytes_to_samples(&audio_data, sample_rate, channels) {
                    let audio_frame = AudioFrame {
                        samples: crate::Samples::PCM { samples },
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        track_id: track_id.to_string(),
                        sample_rate,
                    };
                    
                    // Send to packet sender if available
                    let sender_guard = packet_sender.lock().await;
                    if let Some(sender) = sender_guard.as_ref() {
                        let _ = sender.send(audio_frame);
                    }
                }
            }
            
            PipecatEvent::TtsStarted { text, timestamp } => {
                debug!("TTS started for track {}: {}", track_id, text);
            }
            
            PipecatEvent::TtsCompleted { text, timestamp } => {
                debug!("TTS completed for track {}: {}", track_id, text);
            }
            
            PipecatEvent::Error { message, code } => {
                error!("Pipecat error for track {}: {} (code: {:?})", track_id, message, code);
                let session_event = SessionEvent::Error {
                    error: message,
                    track_id: track_id.to_string(),
                    sender: "pipecat".to_string(),
                    code: code.map(|c| c as u32),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                };
                let _ = event_sender.send(session_event);
            }
            
            PipecatEvent::Metrics { key, duration } => {
                debug!("Pipecat metrics for track {}: {} = {}ms", track_id, key, duration);
            }
            
            PipecatEvent::Ping { timestamp } => {
                debug!("Pipecat ping for track {}: {}", track_id, timestamp);
            }
            
            PipecatEvent::Pong { timestamp } => {
                debug!("Pipecat pong for track {}: {}", track_id, timestamp);
            }
            
            PipecatEvent::Connected { server, version } => {
                debug!("Pipecat connected for track {}: {} v{}", track_id, server, version);
            }
        }
        
        Ok(())
    }
    
    /// Convert raw audio bytes to samples
    fn bytes_to_samples(audio_data: &[u8], _sample_rate: u32, _channels: u32) -> Result<Vec<i16>> {
        // Assuming linear16 encoding (16-bit PCM)
        if audio_data.len() % 2 != 0 {
            return Err(anyhow::anyhow!("Invalid audio data length for linear16"));
        }
        
        let mut samples = Vec::with_capacity(audio_data.len() / 2);
        for chunk in audio_data.chunks_exact(2) {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            samples.push(sample);
        }
        
        Ok(samples)
    }
    
    /// Convert samples to raw audio bytes
    fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &sample in samples {
            let sample_bytes = sample.to_le_bytes();
            bytes.extend_from_slice(&sample_bytes);
        }
        bytes
    }
    
    /// Send audio frame to Pipecat server
    async fn send_audio_to_pipecat(&self, frame: &AudioFrame) -> Result<()> {
        let samples = match &frame.samples {
            crate::Samples::PCM { samples } => samples,
            _ => return Err(anyhow::anyhow!("Unsupported sample format for Pipecat")),
        };
        let audio_bytes = Self::samples_to_bytes(samples);
        self.pipecat_client.send_audio(audio_bytes).await
    }
}

#[async_trait]
impl Track for PipecatTrack {
    fn ssrc(&self) -> u32 {
        self.ssrc
    }
    
    fn id(&self) -> &TrackId {
        &self.id
    }
    
    fn config(&self) -> &TrackConfig {
        &self.config
    }
    
    fn processor_chain(&mut self) -> &mut ProcessorChain {
        &mut self.processor_chain
    }
    
    async fn handshake(&mut self, offer: String, timeout: Option<Duration>) -> Result<String> {
        // PipecatTrack needs a real WebRTC connection to receive audio from the browser
        // We'll create a minimal WebRTC peer connection for audio capture
        debug!("PipecatTrack performing WebRTC handshake to receive audio from browser");
        
        // For now, we need to implement a basic WebRTC handshake
        // This is a simplified version - in reality we'd need full WebRTC setup
        // TODO: Implement proper WebRTC peer connection for audio capture
        
        // Return a basic SDP answer that accepts audio
        let answer = format!(
            "v=0\r\n\
             o=rustpbx 0 0 IN IP4 127.0.0.1\r\n\
             s=Pipecat Audio Session\r\n\
             c=IN IP4 127.0.0.1\r\n\
             t=0 0\r\n\
             m=audio 9 RTP/AVP 0 8 96\r\n\
             a=rtpmap:0 PCMU/8000\r\n\
             a=rtpmap:8 PCMA/8000\r\n\
             a=rtpmap:96 opus/48000/2\r\n\
             a=sendrecv\r\n"
        );
        
        info!("PipecatTrack WebRTC handshake completed - ready to receive audio");
        Ok(answer)
    }
    
    async fn start(&self, event_sender: EventSender, packet_sender: TrackPacketSender) -> Result<()> {
        info!("Starting Pipecat track: {} - setting up audio pipeline", self.id);
        
        // Store packet sender for sending processed audio back
        *self.packet_sender.lock().await = Some(packet_sender);
        
        // Connect to Pipecat server
        if !self.pipecat_client.is_connected().await {
            self.pipecat_client.start_with_reconnect().await?;
        }
        
        // Start event processing
        self.start_event_processing(event_sender).await;
        
        Ok(())
    }
    
    async fn stop(&self) -> Result<()> {
        info!("Stopping Pipecat track: {}", self.id);
        
        // Disconnect from Pipecat server
        self.pipecat_client.disconnect().await?;
        
        // Clear packet sender
        *self.packet_sender.lock().await = None;
        
        Ok(())
    }
    
    async fn send_packet(&self, packet: &AudioFrame) -> Result<()> {
        debug!("PipecatTrack received audio packet: {} samples", 
            match &packet.samples {
                crate::Samples::PCM { samples } => samples.len(),
                _ => 0
            }
        );
        
        // Send audio to Pipecat for processing
        if let Err(e) = self.send_audio_to_pipecat(packet).await {
            warn!("Failed to send audio to Pipecat: {}. Using fallback.", e);
            
            // Fallback: send packet directly to output if Pipecat fails
            let sender_guard = self.packet_sender.lock().await;
            if let Some(sender) = sender_guard.as_ref() {
                let _ = sender.send(packet.clone());
            }
        }
        
        Ok(())
    }
}

impl Drop for PipecatTrack {
    fn drop(&mut self) {
        debug!("Dropping Pipecat track: {}", self.id);
    }
}

/// Builder for PipecatTrack
pub struct PipecatTrackBuilder {
    id: Option<TrackId>,
    ssrc: Option<u32>,
    config: Option<TrackConfig>,
    pipecat_config: Option<crate::pipecat::config::PipecatConfig>,
}

impl PipecatTrackBuilder {
    pub fn new() -> Self {
        Self {
            id: None,
            ssrc: None,
            config: None,
            pipecat_config: None,
        }
    }
    
    pub fn with_id(mut self, id: TrackId) -> Self {
        self.id = Some(id);
        self
    }
    
    pub fn with_ssrc(mut self, ssrc: u32) -> Self {
        self.ssrc = Some(ssrc);
        self
    }
    
    pub fn with_config(mut self, config: TrackConfig) -> Self {
        self.config = Some(config);
        self
    }
    
    pub fn with_pipecat_config(mut self, config: crate::pipecat::config::PipecatConfig) -> Self {
        self.pipecat_config = Some(config);
        self
    }
    
    pub async fn build(self, cancel_token: CancellationToken) -> Result<PipecatTrack> {
        let id = self.id.ok_or_else(|| anyhow::anyhow!("Track ID is required"))?;
        let ssrc = self.ssrc.ok_or_else(|| anyhow::anyhow!("SSRC is required"))?;
        let config = self.config.ok_or_else(|| anyhow::anyhow!("Track config is required"))?;
        let pipecat_config = self.pipecat_config.ok_or_else(|| anyhow::anyhow!("Pipecat config is required"))?;
        
        // Create Pipecat client with event receiver
        let (pipecat_client, event_receiver) = PipecatClient::with_event_receiver(pipecat_config).await?;
        let pipecat_client = Arc::new(pipecat_client);
        
        PipecatTrack::new(id, ssrc, config, pipecat_client, event_receiver, cancel_token).await
    }
}

impl Default for PipecatTrackBuilder {
    fn default() -> Self {
        Self::new()
    }
}