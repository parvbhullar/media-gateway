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
        codecs::{Decoder, pcmu::PcmuDecoder, pcma::PcmaDecoder, g722::G722Decoder, resample::resample_mono},
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
    is_connection_started: Arc<Mutex<bool>>,
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
            is_connection_started: Arc::new(Mutex::new(false)),
        };

        Ok(processor)
    }

    /// Start connection to Pipecat server (called once)
    async fn ensure_connected(&self) {
        let mut is_started = self.is_connection_started.lock().await;
        if *is_started {
            return; // Already started connection attempt
        }
        *is_started = true;
        drop(is_started);

        let pipecat_client = self.pipecat_client.clone();
        let cancel_token = self.cancel_token.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    debug!("Pipecat connection attempt cancelled");
                }
                _ = async {
                    if pipecat_client.is_connected().await {
                        debug!("Pipecat already connected");
                        return;
                    }

                    info!("Starting connection to Pipecat server...");
                    match pipecat_client.start_with_reconnect().await {
                        Ok(_) => {
                            info!("Successfully connected to Pipecat server");
                        }
                        Err(e) => {
                            error!("Failed to connect to Pipecat server: {}", e);
                            // Connection will be retried on next frame if needed
                        }
                    }
                } => {}
            }
        });
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

        info!("Starting Pipecat event processing loop for track: {}", track_id);

        let event_receiver = self.event_receiver.clone();
        let cancel_token = self.cancel_token.clone();
        let packet_sender = self.packet_sender.clone();
        let track_id_clone = track_id.clone();

        tokio::spawn(async move {
            let receiver = {
                let mut guard = event_receiver.lock().await;
                guard.take()
            };

            if let Some(mut rx) = receiver {
                info!("Pipecat event receiver loop started for track {}", track_id_clone);
                let mut event_count = 0u64;

                loop {
                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            info!("Pipecat event processing cancelled for track {} (processed {} events)", track_id_clone, event_count);
                            break;
                        }
                        event = rx.recv() => {
                            match event {
                                Some(pipecat_event) => {
                                    event_count += 1;

                                    // Handle AudioResponse events (the main event we care about)
                                    match pipecat_event {
                                        PipecatEvent::AudioResponse { audio_data, sample_rate, channels } => {
                                            // Convert raw audio bytes to samples and create audio frame for playback
                                            match Self::bytes_to_samples(&audio_data, sample_rate, channels) {
                                                Ok(samples) => {
                                                    info!(
                                                        "Received audio response from Pipecat: {} bytes, {} samples, {}Hz, {} channels",
                                                        audio_data.len(), samples.len(), sample_rate, channels
                                                    );

                                                    let audio_frame = AudioFrame {
                                                        samples: crate::Samples::PCM { samples },
                                                        timestamp: std::time::SystemTime::now()
                                                            .duration_since(std::time::UNIX_EPOCH)
                                                            .unwrap()
                                                            .as_millis() as u64,
                                                        track_id: track_id_clone.clone(),
                                                        sample_rate,
                                                    };

                                                    // Send to packet sender for playback on WebRTC track
                                                    let sender_guard = packet_sender.lock().await;
                                                    if let Some(sender) = sender_guard.as_ref() {
                                                        match sender.send(audio_frame) {
                                                            Ok(_) => {
                                                                info!("Successfully sent audio response to WebRTC track (event #{})", event_count);
                                                            }
                                                            Err(e) => {
                                                                error!("Failed to send audio response to track: {}", e);
                                                            }
                                                        }
                                                    } else {
                                                        warn!("No packet sender available for audio playback");
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Failed to convert audio response bytes to samples: {}", e);
                                                }
                                            }
                                        }
                                        PipecatEvent::TranscriptionDelta { text, timestamp } => {
                                            debug!("STT delta for track {}: {} (ts: {})", track_id_clone, text, timestamp);
                                        }
                                        PipecatEvent::TranscriptionFinal { text, timestamp } => {
                                            info!("STT final for track {}: {} (ts: {})", track_id_clone, text, timestamp);
                                        }
                                        PipecatEvent::LlmResponse { text, is_complete, .. } => {
                                            debug!("LLM response for track {}: {} (complete: {})", track_id_clone, text, is_complete);
                                        }
                                        PipecatEvent::TtsStarted { text, .. } => {
                                            info!("TTS started for track {}: {}", track_id_clone, text);
                                        }
                                        PipecatEvent::TtsCompleted { text, .. } => {
                                            info!("TTS completed for track {}: {}", track_id_clone, text);
                                        }
                                        _ => {
                                            debug!("Received Pipecat event #{}: {:?}", event_count, pipecat_event);
                                        }
                                    }
                                }
                                None => {
                                    warn!("Pipecat event channel closed for track {} after {} events", track_id_clone, event_count);
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                warn!("No event receiver available for track {}", track_id_clone);
            }
        });
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
                info!("Starting Pipecat event processing loop for track {}", track_id);
                let mut event_count = 0u64;

                loop {
                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            info!("Pipecat event processing cancelled for track {} (processed {} events)", track_id, event_count);
                            break;
                        }
                        event = rx.recv() => {
                            match event {
                                Some(pipecat_event) => {
                                    event_count += 1;
                                    if let Err(e) = Self::handle_pipecat_event(
                                        pipecat_event,
                                        &event_sender,
                                        &track_id,
                                        &packet_sender,
                                    ).await {
                                        error!("Error handling Pipecat event #{}: {}", event_count, e);
                                    }
                                }
                                None => {
                                    warn!("Pipecat event channel closed for track {} after {} events", track_id, event_count);
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                warn!("No event receiver available for track {}", track_id);
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
                // Convert raw audio bytes to samples and create audio frame for playback
                match Self::bytes_to_samples(&audio_data, sample_rate, channels) {
                    Ok(samples) => {
                        info!(
                            "Received audio response from Pipecat: {} bytes, {} samples, {}Hz, {} channels",
                            audio_data.len(), samples.len(), sample_rate, channels
                        );

                        let audio_frame = AudioFrame {
                            samples: crate::Samples::PCM { samples },
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64,
                            track_id: track_id.to_string(),
                            sample_rate,
                        };

                        // Send to packet sender for playback on WebRTC track
                        let sender_guard = packet_sender.lock().await;
                        if let Some(sender) = sender_guard.as_ref() {
                            match sender.send(audio_frame) {
                                Ok(_) => {
                                    debug!("Successfully sent audio response to WebRTC track");
                                }
                                Err(e) => {
                                    error!("Failed to send audio response to track: {}", e);
                                }
                            }
                        } else {
                            warn!("No packet sender available for audio playback");
                        }
                    }
                    Err(e) => {
                        error!("Failed to convert audio response bytes to samples: {}", e);
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
        // Pipecat server expects 16kHz PCM audio
        const PIPECAT_SAMPLE_RATE: u32 = 16000;

        // Get the source sample rate from the frame
        let source_sample_rate = frame.sample_rate;

        // Convert different audio formats to PCM samples for Pipecat
        let samples = match &frame.samples {
            crate::Samples::PCM { samples } => {
                // PCM samples - use frame's sample_rate
                samples.clone()
            }
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

        // Resample to 16kHz if needed (use frame's sample_rate for accurate resampling)
        let resampled_samples = if source_sample_rate != PIPECAT_SAMPLE_RATE {
            debug!("Resampling audio from {}Hz to {}Hz for Pipecat (frame has {} samples)",
                   source_sample_rate, PIPECAT_SAMPLE_RATE, samples.len());
            resample_mono(&samples, source_sample_rate, PIPECAT_SAMPLE_RATE)
        } else {
            samples
        };

        let audio_bytes = Self::samples_to_bytes(&resampled_samples);
        pipecat_client.send_audio(audio_bytes).await
    }
}

impl Processor for PipecatProcessor {
    fn process_frame(&self, frame: &mut AudioFrame) -> Result<()> {
        // Ensure connection is started (lazy initialization - runs once)
        let processor_ref = self.clone();
        tokio::spawn(async move {
            processor_ref.ensure_connected().await;
        });

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

            // Check if connected - if not, skip this frame (connection is happening in background)
            if !pipecat_client.is_connected().await {
                // Only log occasionally to avoid spam
                use std::sync::atomic::{AtomicU64, Ordering};
                static SKIP_COUNT: AtomicU64 = AtomicU64::new(0);
                let count = SKIP_COUNT.fetch_add(1, Ordering::Relaxed);
                if count % 100 == 0 {
                    debug!("Skipping frame {} - waiting for Pipecat connection", count);
                }
                return;
            }

            // Forward the audio frame with error handling
            use std::sync::atomic::{AtomicU64, Ordering};
            static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
            let frame_num = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);

            match Self::forward_audio_to_pipecat(&pipecat_client, &frame_clone).await {
                Ok(_) => {
                    // Log every frame for first 10, then every 50th frame
                    if frame_num < 10 || frame_num % 50 == 0 {
                        let sample_info = match &frame_clone.samples {
                            crate::Samples::PCM { samples } => format!("PCM {} samples", samples.len()),
                            crate::Samples::RTP { payload_type, payload, .. } => format!("RTP pt:{} {} bytes", payload_type, payload.len()),
                            _ => "Unknown".to_string(),
                        };
                        info!("🎤 Forwarded audio frame #{} to Pipecat: {} @ {}Hz, track: {}",
                            frame_num, sample_info, frame_clone.sample_rate, frame_clone.track_id);
                    }
                }
                Err(e) => {
                    // Log error but don't fail the entire processing pipeline
                    static ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
                    let count = ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
                    if count % 50 == 0 {
                        warn!("❌ Failed to forward audio frame #{} to Pipecat (error #{}, non-fatal): {}", frame_num, count, e);
                    }

                    // Check if we should attempt reconnection
                    if !pipecat_client.is_connected().await {
                        debug!("Pipecat connection lost, reconnection will be attempted");
                    }
                }
            }
        });

        // Continue processing normally - don't modify the original frame
        // This ensures that even if Pipecat fails, the audio pipeline continues
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
