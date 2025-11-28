/*!
 * Pipecat Media Processor for RustPBX
 *
 * This processor intercepts audio from WebRTC tracks and forwards it to the
 * Pipecat media server for AI processing while allowing normal audio flow to continue.
 */

use anyhow::Result;
use std::{pin::Pin, sync::Arc, time::Duration, future::Future,};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    AudioFrame, TrackId,
    event::{EventSender, SessionEvent},
    media::{
        codecs::{
            Decoder, g722::G722Decoder, pcma::PcmaDecoder, pcmu::PcmuDecoder,
            resample::resample_mono,
        },
        processor::Processor,
        track::TrackPacketSender,
    },
    pipecat::{PipecatClient, PipecatConfig, PipecatEvent, PipecatEventReceiver},
};

#[cfg(feature = "opus")]
use crate::media::codecs::opus::OpusDecoder;

/// Pipecat media processor that intercepts audio and forwards it to the Pipecat server
#[derive(Clone)]
pub struct PipecatProcessor {
    pipecat_client: Arc<PipecatClient>,
    cancel_token: CancellationToken,
    //is_event_processing_started: Arc<Mutex<bool>>,
    //is_connection_started: Arc<Mutex<bool>>,
    //audio_playback_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>>>>,
}

impl PipecatProcessor {
    pub fn create(
        track_id: TrackId,
        cancel_token: CancellationToken,
        pipecat_config: PipecatConfig,
        event_sender: EventSender,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Processor>>> + Send>> {
        Box::pin(async move {
            info!("Creating Pipecat processor for track: {}", track_id);

            // Create Pipecat client
            let (pipecat_client, event_receiver) =
                PipecatClient::with_event_receiver(pipecat_config).await?;

            // Connect to Pipecat server
            pipecat_client.connect().await?;

            // Wait for connection to be established
            let mut attempts = 0;
            while !pipecat_client.is_connected().await && attempts < 20 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                attempts += 1;
            }

            if !pipecat_client.is_connected().await {
                return Err(anyhow::anyhow!(
                    "Failed to establish Pipecat connection after 2 seconds"
                ));
            }

            info!("✅ Pipecat connection established for track: {}", track_id);

            // Create the processor
            let processor = Self::new(
                pipecat_client.into(),
                event_receiver,
                cancel_token,
                event_sender,
                track_id,
            )
            .await?;

            Ok(Box::new(processor) as Box<dyn Processor>)
        })
    }

    fn spawn_event_processor(
        mut event_receiver: PipecatEventReceiver,
        cancel_token: CancellationToken,
        event_sender: EventSender,
        track_id: String,
        //audio_tx: tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
    ) {
        tokio::spawn(async move {
            info!("🎧 Starting Pipecat event processor for track {}", track_id);
            let mut event_count = 0u64;

            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        info!("Pipecat event processing cancelled (processed {} events)", event_count);
                        break;
                    }
                    event = event_receiver.recv() => {
                        match event {
                            Some(pipecat_event) => {
                                event_count += 1;

                                match &pipecat_event {
                                    PipecatEvent::AudioResponse { audio_data, sample_rate, channels } => {
                                        debug!("🎵 Received {} bytes from Pipecat ({}Hz, {} ch)",
                                            audio_data.len(), sample_rate, channels);

                                        // Convert to samples
                                        let samples = Self::bytes_to_samples(audio_data, *sample_rate, *channels)
                                            .unwrap_or_else(|e| {
                                                error!("Failed to convert audio: {}", e);
                                                vec![]
                                            });

                                            if !samples.is_empty() {
                                                info!("🔊 Queueing {} samples for playback to user", samples.len());
                                                
                                                // ✅ Send via event - DO NOT use disconnected channel
                                                if let Err(e) = event_sender.send(SessionEvent::PipecatAudio {
                                                    track_id: "server-side-track".to_string(),
                                                    audio_samples: samples,
                                                    sample_rate: *sample_rate,
                                                    timestamp: crate::get_timestamp(),
                                                }) {
                                                    error!("Failed to send PipecatAudio event: {}", e);
                                                }
                                            }
                                    }

                                    PipecatEvent::TranscriptionFinal { text, timestamp } => {
                                        info!("📝 Pipecat STT (final): {}", text);
                                        let _ = event_sender.send(SessionEvent::AsrFinal {
                                            text: text.clone(),
                                            track_id: track_id.clone(),
                                            timestamp: *timestamp,
                                            start_time: Some(*timestamp),
                                            end_time: Some(*timestamp),
                                            index: 0,
                                        });
                                    }

                                    PipecatEvent::TranscriptionDelta { text, timestamp } => {
                                        debug!("📝 Pipecat STT (delta): {}", text);
                                        let _ = event_sender.send(SessionEvent::AsrDelta {
                                            text: text.clone(),
                                            track_id: track_id.clone(),
                                            timestamp: *timestamp,
                                            start_time: Some(*timestamp),
                                            end_time: Some(*timestamp),
                                            index: 0,
                                        });
                                    }

                                    PipecatEvent::Error { message, code } => {
                                        error!("❌ Pipecat error: {}", message);
                                        let _ = event_sender.send(SessionEvent::Error {
                                            error: message.clone(),
                                            track_id: track_id.clone(),
                                            sender: "pipecat".to_string(),
                                            code: code.map(|c| c as u32),
                                            timestamp: crate::get_timestamp(),
                                        });
                                    }

                                    _ => {
                                        debug!("📨 Pipecat event: {:?}", pipecat_event);
                                    }
                                }
                            }
                            None => {
                                warn!("Pipecat event channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Create a new Pipecat processor
    pub async fn new(
        pipecat_client: Arc<PipecatClient>,
        event_receiver: PipecatEventReceiver,
        cancel_token: CancellationToken,
        event_sender: EventSender,
        track_id: String,
    ) -> Result<Self> {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();

        // ✅ Start event processing immediately
        Self::spawn_event_processor(
            event_receiver,
            cancel_token.clone(),
            event_sender,
            track_id,
            //audio_tx,
        );

        Ok(Self {
            pipecat_client,
            cancel_token,
            //is_event_processing_started: Arc::new(Mutex::new(true)), // Already started
            //is_connection_started: Arc::new(Mutex::new(true)),       // Already connected
            //audio_playback_rx: Arc::new(Mutex::new(Some(audio_rx))),
        })
    }

    /// Start connection to Pipecat server (called once)
    async fn ensure_connected(&self) {
        // let mut is_started = self.is_connection_started.lock().await;
        // if *is_started {
        //     return; // Already started connection attempt
        // }
        // *is_started = true;
        // drop(is_started);

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
    async fn forward_audio_to_pipecat(
        pipecat_client: &PipecatClient,
        frame: &AudioFrame,
    ) -> Result<()> {
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
            crate::Samples::RTP {
                payload_type,
                payload,
                ..
            } => {
                // Decode RTP payload to PCM samples based on payload type
                match *payload_type {
                    0 => {
                        // PCMU
                        let mut decoder = PcmuDecoder::new();
                        decoder.decode(payload)
                    }
                    8 => {
                        // PCMA
                        let mut decoder = PcmaDecoder::new();
                        decoder.decode(payload)
                    }
                    9 => {
                        // G.722
                        let mut decoder = G722Decoder::new();
                        decoder.decode(payload)
                    }
                    111 => {
                        // Opus
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
                        return Err(anyhow::anyhow!(
                            "Unsupported payload type: {}",
                            payload_type
                        ));
                    }
                }
            }
            crate::Samples::Empty => {
                return Ok(()); // Nothing to send for empty samples
            }
        };

        // Resample to 16kHz if needed (use frame's sample_rate for accurate resampling)
        let resampled_samples = if source_sample_rate != PIPECAT_SAMPLE_RATE {
            debug!(
                "Resampling audio from {}Hz to {}Hz for Pipecat (frame has {} samples)",
                source_sample_rate,
                PIPECAT_SAMPLE_RATE,
                samples.len()
            );
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
        // if let Ok(mut rx_guard) = self.audio_playback_rx.try_lock() {
        //     if let Some(rx) = rx_guard.as_mut() {
        //         // Non-blocking check for Pipecat audio
        //         match rx.try_recv() {
        //             Ok(pipecat_samples) => {
        //                 info!(
        //                     "🔊 Injecting {} samples from Pipecat into frame for track {}",
        //                     pipecat_samples.len(),
        //                     frame.track_id
        //                 );

        //                 // ✅ Replace the entire frame with Pipecat's audio response
        //                 frame.samples = crate::Samples::PCM {
        //                     samples: pipecat_samples,
        //                 };
        //                 frame.timestamp = crate::get_timestamp();
        //                 frame.sample_rate = 16000;

        //                 // ✅ Return immediately - send Pipecat audio to user
        //                 return Ok(());
        //             }
        //             Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
        //                 // No audio from Pipecat right now - continue normally
        //             }
        //             Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
        //                 warn!(
        //                     "⚠️ Pipecat audio playback channel disconnected for track {}",
        //                     frame.track_id
        //                 );
        //             }
        //         }
        //     }
        // }

        // Forward audio to Pipecat for AI processing (non-blocking)
        let pipecat_client = self.pipecat_client.clone();
        let frame_clone = frame.clone();

        tokio::spawn(async move {
            // Forward the audio frame with error handling
            let is_connected = pipecat_client.is_connected().await;
            let connection_status = pipecat_client.get_status().await;
            use std::sync::atomic::{AtomicU64, Ordering};
            static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
            let frame_num = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);

            if !is_connected {
                warn!(
                    "❌ Frame #{} skipped - not connected to Pipecat",
                    FRAME_COUNT.load(Ordering::Relaxed)
                );
                return;
            }

            match Self::forward_audio_to_pipecat(&pipecat_client, &frame_clone).await {
                Ok(_) => {
                    // Log every frame for first 10, then every 50th frame
                    if frame_num < 10 || frame_num % 50 == 0 {
                        let sample_info = match &frame_clone.samples {
                            crate::Samples::PCM { samples } => {
                                format!("PCM {} samples", samples.len())
                            }
                            crate::Samples::RTP {
                                payload_type,
                                payload,
                                ..
                            } => format!("RTP pt:{} {} bytes", payload_type, payload.len()),
                            _ => "Unknown".to_string(),
                        };
                        info!(
                            "🎤 Forwarded audio frame #{} to Pipecat: {} @ {}Hz, track: {}",
                            frame_num, sample_info, frame_clone.sample_rate, frame_clone.track_id
                        );
                    }
                }
                Err(e) => {
                    // Log error but don't fail the entire processing pipeline
                    static ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
                    let count = ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
                    let error_msg = e.to_string();
                    if error_msg.contains("Not connected") || error_msg.contains("not connected") {
                        // Expected during connection phase - only log occasionally
                        if count % 100 == 0 {
                            debug!(
                                "Waiting for Pipecat connection (frame #{}, error #{})",
                                frame_num, count
                            );
                        }
                    } else {
                        // Unexpected error - log it
                        if count % 50 == 0 {
                            warn!(
                                "❌ Failed to forward audio frame #{} to Pipecat (error #{}, non-fatal): {}",
                                frame_num, count, e
                            );
                        }
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

    pub async fn build(
        self,
        cancel_token: CancellationToken,
        event_sender: EventSender, // ✅ Add this parameter
        track_id: String,          // ✅ Add this parameter
    ) -> Result<PipecatProcessor> {
        let pipecat_config = self
            .pipecat_config
            .ok_or_else(|| anyhow::anyhow!("Pipecat config is required"))?;

        // Create Pipecat client with event receiver
        let (pipecat_client, event_receiver) =
            PipecatClient::with_event_receiver(pipecat_config).await?;
        let pipecat_client = Arc::new(pipecat_client);

        // ✅ Call new() with all 5 required arguments
        PipecatProcessor::new(
            pipecat_client,
            event_receiver,
            cancel_token,
            event_sender, // ✅ Pass event_sender
            track_id,     // ✅ Pass track_id
        )
        .await
    }
}

impl Default for PipecatProcessorBuilder {
    fn default() -> Self {
        Self::new()
    }
}
