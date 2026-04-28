//! Background webhook delivery processor (WH-02..WH-04).
//!
//! Phase 7 Plan 07-01 ships the function signature only. The body lands
//! in 07-04 (D-12 — fresh DB read per event, per-webhook spawn, retry
//! schedule with jitter, disk fallback on terminal fail).

use std::sync::Arc;

use sea_orm::DatabaseConnection;
use tokio_util::sync::CancellationToken;

use super::{WebhookCancelRegistry, WebhookEventSender};

/// Run the webhook processor until `cancel` fires. Spawned at server
/// boot. Body lands in 07-04.
pub async fn run_webhook_processor(
    _db: DatabaseConnection,
    _sender: WebhookEventSender,
    _cancel_registry: Arc<WebhookCancelRegistry>,
    _generated_dir: String,
    _cancel: CancellationToken,
) {
    // Body lands in Plan 07-04.
}
