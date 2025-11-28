use crate::{AudioFrame, TrackId, event};
use crate::media::track::{Track, TrackConfig};
use crate::media::processor::ProcessorChain;
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// A simple track that accepts injected audio frames and sends them to WebRTC
/// Used for server-side audio playback (Pipecat TTS, TTS synthesis, etc.)
pub struct PassthroughTrack {
    track_id: TrackId,
    ssrc: u32,
    cancel_token: CancellationToken,
    packet_sender: mpsc::UnboundedSender<AudioFrame>,
    _packet_receiver: mpsc::UnboundedReceiver<AudioFrame>,
    config: TrackConfig,
    processor_chain: ProcessorChain,
}

impl PassthroughTrack {
    pub fn new(track_id: TrackId, ssrc: u32, cancel_token: CancellationToken) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        
        Self {
            track_id,
            ssrc,
            cancel_token,
            packet_sender: tx,
            _packet_receiver: rx,
            config: TrackConfig::default(),
            processor_chain: ProcessorChain::new(16000),
        }
    }
}

#[async_trait]
impl Track for PassthroughTrack {
    fn id(&self) -> &std::string::String {
        &self.track_id
    }

    fn ssrc(&self) -> u32 {
        self.ssrc
    }

    fn config(&self) -> &TrackConfig {
        &self.config
    }

    fn processor_chain(&mut self) -> &mut ProcessorChain {
        &mut self.processor_chain
    }

    async fn handshake(
        &mut self,
        _offer: String,
        _timeout: Option<std::time::Duration>,
    ) -> Result<String> {
        // No handshake needed for passthrough track
        Ok(String::new())
    }

    async fn start(
        &self,
        _event_sender: event::EventSender,
        _packet_sender: crate::media::track::TrackPacketSender,
    ) -> Result<()> {
        info!("🔄 PassthroughTrack starting: {}", self.track_id);
        // Use event_sender and packet_sender as needed
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        info!("🔄 PassthroughTrack stopping: {}", self.track_id);
        Ok(())
    }

    async fn send_packet(&self, _packet: &AudioFrame) -> Result<()> {
        // Packets are sent via packet_sender, not this method
        Ok(())
    }
}