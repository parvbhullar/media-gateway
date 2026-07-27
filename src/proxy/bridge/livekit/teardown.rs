//! LiveKitTeardown — leaves the room and optionally deletes it on SIP BYE.
//! Implements BridgeTeardown.

use std::sync::Arc;

use async_trait::async_trait;
use livekit::Room;
use livekit_api::services::room::RoomClient;

use crate::proxy::bridge::session::{BridgeTeardown, TeardownError};

/// Teardown for the livekit kind. Holds the connected `Room` plus the
/// admin `RoomClient` so the optional `delete_room_on_hangup` flag can
/// drive a server-side DeleteRoom RPC.
///
/// `room` is shared with `LiveKitBridge` via `Arc` because `livekit::Room`
/// is not `Clone` — both halves need to observe the same session.
pub struct LiveKitTeardown {
    pub room: Arc<Room>,
    pub room_service: Option<RoomClient>,
    pub room_name: String,
    pub delete_room_on_hangup: bool,
}

#[async_trait]
impl BridgeTeardown for LiveKitTeardown {
    async fn close(&self) -> Result<(), TeardownError> {
        // 1. Disconnect our participant. Errors non-fatal — the server may
        //    have already torn the room down (BYE arrives after remote drop).
        if let Err(e) = self.room.close().await {
            tracing::warn!(error = %e, room = %self.room_name,
                "livekit room.close failed (may already be disconnected)");
        }

        // 2. Optional admin-side delete via the RoomService Twirp RPC.
        if self.delete_room_on_hangup {
            if let Some(svc) = &self.room_service {
                if let Err(e) = svc.delete_room(&self.room_name).await {
                    return Err(TeardownError::Remote(format!(
                        "delete_room({}) failed: {e}",
                        self.room_name
                    )));
                }
            } else {
                tracing::warn!(room = %self.room_name,
                    "delete_room_on_hangup=true but no room_service client supplied; skipping");
            }
        }
        Ok(())
    }
}
