/*!
 * Pipecat Media Processor for RustPBX
 * 
 * This processor intercepts audio from WebRTC tracks and forwards it to the 
 * Pipecat media server for AI processing while allowing normal audio flow to continue.
 */

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    AudioFrame,
    event::{EventSender, SessionEvent},
    media::{
        codecs::{Decoder, pcmu::PcmuDecoder, pcma::PcmaDecoder, g722::G722Decoder},
        processor::Processor,
        track::TrackPacketSender,
    },
    pipecat::{PipecatClient, PipecatEvent, PipecatEventReceiver},
};

#[cfg(feature = "opus")]
use crate::media::codecs::opus::OpusDecoder;

/// Pipecat media processor that intercepts audio and forwards it to the Pipecat server
#[derive(Clone)]
pub struct PipecatProcessor {
    pipecat_client: Arc<PipecatClient>,
    event_receiver: Arc<Mutex<Option<PipecatEventReceiver>>>,
    packet_sender: Arc<Mutex<Option<TrackPacketSender>>>,
    cancel_token: CancellationToken,
    is_event_processing_started: Arc<Mutex<bool>>,
}

impl PipecatProcessor {
    /// Create a new Pipecat processor
    pub async fn new(
        pipecat_client: Arc<PipecatClient>,
        event_receiver: PipecatEventReceiver,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let processor = Self {
            pipecat_client,
            event_receiver: Arc::new(Mutex::new(Some(event_receiver))),
            packet_sender: Arc::new(Mutex::new(None)),
            cancel_token,
            is_event_processing_started: Arc::new(Mutex::new(false)),
        };
        
        Ok(processor)
    }
    
    /// Set the packet sender for sending processed audio back
    pub async fn set_packet_sender(&self, packet_sender: TrackPacketSender) {
        *self.packet_sender.lock().await = Some(packet_sender);
    }
    
    /// Start event processing once (called from process_frame)
    async fn start_event_processing_once(&self, track_id: String) {
        let mut is_started = self.is_event_processing_started.lock().await;
        if *is_started {
            return; // Already started
        }
        *is_started = true;
        drop(is_started);
        
        info!("Starting Pipecat event processing for track: {}", track_id);
        
        // We don't have an event_sender in the processor context
        // Event handling will be done differently - through the packet_sender for audio responses
        // For now, we'll skip event processing as the main goal is audio forwarding
        debug!("Pipecat event processing initialized for track: {}", track_id);
    }
    
    /// Start processing events from Pipecat
    async fn start_event_processing(&self, event_sender: EventSender, track_id: String) {
        // Check if event processing is already started
        let mut is_started = self.is_event_processing_started.lock().await;
        if *is_started {
            debug!("Event processing already started for track {}", track_id);
            return;
        }
        *is_started = true;
        drop(is_started);
        
        let event_receiver = self.event_receiver.clone();
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
            
            PipecatEvent::LlmResponse { text, is_complete, timestamp: _ } => {
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
            
            PipecatEvent::TtsStarted { text, timestamp: _ } => {
                debug!("TTS started for track {}: {}", track_id, text);
            }
            
            PipecatEvent::TtsCompleted { text, timestamp: _ } => {
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
    
    /// Forward audio frame to Pipecat server (static method for use in async tasks)
    async fn forward_audio_to_pipecat(pipecat_client: &PipecatClient, frame: &AudioFrame) -> Result<()> {
        // Convert different audio formats to PCM samples for Pipecat
        let samples = match &frame.samples {
            crate::Samples::PCM { samples } => samples.clone(),
            crate::Samples::RTP { payload_type, payload, .. } => {
                // Decode RTP payload to PCM samples based on payload type
                match *payload_type {
                    0 => {  // PCMU
                        let mut decoder = PcmuDecoder::new();
                        decoder.decode(payload)
                    }
                    8 => {  // PCMA
                        let mut decoder = PcmaDecoder::new();
                        decoder.decode(payload)
                    }
                    9 => {  // G.722
                        let mut decoder = G722Decoder::new();
                        decoder.decode(payload)
                    }
                    111 => { // Opus
                        #[cfg(feature = "opus")]
                        {
                            let mut decoder = OpusDecoder::new_default();
                            decoder.decode(payload)
                        }
                        #[cfg(not(feature = "opus"))]
                        {
                            return Err(anyhow::anyhow!("Opus codec not enabled"));
                        }
                    }
                    _ => {
                        return Err(anyhow::anyhow!("Unsupported payload type: {}", payload_type));
                    }
                }
            }
            crate::Samples::Empty => {
                return Ok(()); // Nothing to send for empty samples
            }
        };
        
        let audio_bytes = Self::samples_to_bytes(&samples);
        pipecat_client.send_audio(audio_bytes).await
    }
}

impl Processor for PipecatProcessor {
    fn process_frame(&self, frame: &mut AudioFrame) -> Result<()> {
        // Start event processing once (lazy initialization)
        let track_id = frame.track_id.clone();
        let processor_ref = self.clone();
        tokio::spawn(async move {
            processor_ref.start_event_processing_once(track_id).await;
        });
        
        // Forward audio to Pipecat for AI processing (non-blocking)
        let pipecat_client = self.pipecat_client.clone();
        let frame_clone = frame.clone();
        let cancel_token = self.cancel_token.clone();
        
        tokio::spawn(async move {
            // Don't use cancel token for individual audio frames - they should be processed
            // Only check if we're explicitly shutting down the processor
            if cancel_token.is_cancelled() {
                debug!("Processor is shutting down, skipping audio forwarding");
                return;
            }
            
            // Connect to Pipecat server if not already connected
            if !pipecat_client.is_connected().await {
                info!("Connecting to Pipecat server for audio processing");
                if let Err(e) = pipecat_client.start_with_reconnect().await {
                    warn!("Failed to connect to Pipecat server: {}", e);
                    return; // Skip this frame if can't connect
                }
            }
            
            // Forward the audio frame
            if let Err(e) = Self::forward_audio_to_pipecat(&pipecat_client, &frame_clone).await {
                debug!("Failed to forward audio to Pipecat: {}", e);
            } else {
                debug!("Successfully forwarded audio frame to Pipecat server");
            }
        });
        
        // Continue processing normally - don't modify the original frame
        Ok(())
    }
}

/// Builder for PipecatProcessor
pub struct PipecatProcessorBuilder {
    pipecat_config: Option<crate::pipecat::config::PipecatConfig>,
}

impl PipecatProcessorBuilder {
    pub fn new() -> Self {
        Self {
            pipecat_config: None,
        }
    }
    
    pub fn with_pipecat_config(mut self, config: crate::pipecat::config::PipecatConfig) -> Self {
        self.pipecat_config = Some(config);
        self
    }
    
    pub async fn build(self, cancel_token: CancellationToken) -> Result<PipecatProcessor> {
        let pipecat_config = self.pipecat_config.ok_or_else(|| anyhow::anyhow!("Pipecat config is required"))?;
        
        // Create Pipecat client with event receiver
        let (pipecat_client, event_receiver) = PipecatClient::with_event_receiver(pipecat_config).await?;
        let pipecat_client = Arc::new(pipecat_client);
        
        PipecatProcessor::new(pipecat_client, event_receiver, cancel_token).await
    }
}

impl Default for PipecatProcessorBuilder {
    fn default() -> Self {
        Self::new()
    }
}