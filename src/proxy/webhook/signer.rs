//! HMAC-SHA256 webhook payload signer (Stripe-style).
//!
//! Phase 7 Plan 07-01 ships the signature only; the body lands in 07-03
//! (D-14: signed payload format `"{timestamp}.{body}"`, hex-encoded).

/// Compute the `v1=<hex>` portion of the `X-Webhook-Signature` header
/// per D-14 / D-15. Body filled in 07-03.
pub fn sign(_timestamp: i64, _body: &str, _secret: &str) -> String {
    unimplemented!("Phase 7 Plan 07-03 lands the HMAC body")
}
