//! ExternalMediaTeardown — ends the SIP-side dialog by telling the sidecar
//! to leave its far-end session and reaping the spawned process.
//!
//! On SIP BYE we:
//!   1. Send a best-effort `BYE` control datagram over the PCM socket so the
//!      sidecar can leave its LiveKit room cleanly (vs. being SIGKILLed and
//!      leaving the server to reap the participant on a timeout).
//!   2. Give the child a short grace period to exit on its own, then
//!      force-kill it as a backstop.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::UdpSocket;
use tokio::process::Child;

use crate::proxy::bridge::session::{BridgeTeardown, TeardownError};

/// Grace period between the `BYE` datagram and force-killing the sidecar.
const GRACE: Duration = Duration::from_millis(500);

pub struct ExternalMediaTeardown {
    /// The spawned sidecar process. `kill_on_drop(true)` is also set at
    /// spawn time as a final backstop if `close()` is never reached.
    pub child: parking_lot::Mutex<Option<Child>>,
    /// PCM socket, connected to the sidecar — used to send the `BYE`
    /// control datagram.
    pub sock: Arc<UdpSocket>,
    pub trunk_name: String,
}

#[async_trait]
impl BridgeTeardown for ExternalMediaTeardown {
    async fn close(&self) -> Result<(), TeardownError> {
        // 1. Best-effort in-band BYE so the sidecar leaves its room cleanly.
        if let Err(e) = self.sock.send(b"BYE").await {
            tracing::debug!(trunk = %self.trunk_name, error = %e,
                "external_media: BYE datagram send failed (sidecar may be gone)");
        }

        // 2. Grace, then force-kill.
        let child = self.child.lock().take();
        if let Some(mut child) = child {
            tokio::select! {
                status = child.wait() => {
                    tracing::info!(trunk = %self.trunk_name, ?status,
                        "external_media sidecar exited gracefully after BYE");
                }
                _ = tokio::time::sleep(GRACE) => {
                    if let Err(e) = child.start_kill() {
                        return Err(TeardownError::Remote(format!(
                            "failed to kill external_media sidecar: {e}"
                        )));
                    }
                    let _ = child.wait().await;
                    tracing::info!(trunk = %self.trunk_name,
                        "external_media sidecar force-killed after grace period");
                }
            }
        }
        Ok(())
    }
}
