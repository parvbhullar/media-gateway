//! WebRtcTeardown — adapter.close() driver for the BridgeTeardown trait.

use std::sync::Arc;

use async_trait::async_trait;

use crate::proxy::bridge::session::{BridgeTeardown, TeardownError};
use crate::proxy::bridge::signaling::{SessionHandle, SignalingContext, WebRtcSignalingAdapter};

/// Teardown action for the webrtc kind. At SIP BYE time, calls
/// `adapter.close(ctx, session)` to release the bot's signaling
/// session. Stash on the unified `BridgeSession` for kind-agnostic
/// teardown later.
pub struct WebRtcTeardown {
    pub adapter: Arc<dyn WebRtcSignalingAdapter>,
    pub session: SessionHandle,
    pub ctx: SignalingContext,
}

#[async_trait]
impl BridgeTeardown for WebRtcTeardown {
    async fn close(&self) -> Result<(), TeardownError> {
        self.adapter
            .close(&self.ctx, &self.session)
            .await
            .map_err(|e| TeardownError::Remote(e.to_string()))
    }
}
