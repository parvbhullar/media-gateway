//! WebRtcMediaBridge — wraps the existing Arc<BridgePeer> so it satisfies
//! the kind-agnostic MediaBridge trait.

use std::sync::Arc;

use async_trait::async_trait;

use crate::media::bridge::BridgePeer;
use crate::proxy::bridge::session::{BridgeKind, MediaBridge};

pub struct WebRtcMediaBridge(pub Arc<BridgePeer>);

#[async_trait]
impl MediaBridge for WebRtcMediaBridge {
    async fn start(&self) -> anyhow::Result<()> {
        self.0.start_bridge().await;
        Ok(())
    }
    fn kind(&self) -> BridgeKind {
        BridgeKind::WebRtc
    }
    fn attach_recorder(
        &self,
        recorder: Arc<parking_lot::RwLock<Option<crate::media::recorder::Recorder>>>,
    ) {
        self.0.attach_recorder(recorder);
    }
}
