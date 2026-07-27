//! HMAC-SHA256 webhook payload signer (Stripe-style).
//!
//! Per D-14: signed payload is canonicalized as `"{timestamp}.{body}"`,
//! HMAC-SHA256'd with the per-webhook secret, and lowercase hex-encoded.
//! Per D-15: the resulting hex digest is embedded into the
//! `X-Webhook-Signature: t=<ts>,v1=<hex>` header. Header concatenation is
//! the caller's responsibility (07-04 processor) — see `signature_header`
//! convenience helper below.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Stripe-style HMAC-SHA256 over canonical payload `{timestamp}.{body}`.
/// Returns the bare lowercase hex digest (64 chars).
pub fn sign(timestamp: i64, body: &str, secret: &str) -> String {
    let signed_payload = format!("{}.{}", timestamp, body);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(signed_payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Format the full `X-Webhook-Signature` header value per D-15:
/// `t={timestamp},v1={hex_sig}`.
pub fn signature_header(timestamp: i64, body: &str, secret: &str) -> String {
    format!("t={},v1={}", timestamp, sign(timestamp, body, secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference vectors precomputed locally with:
    //   printf '%s' '<payload>' | openssl dgst -sha256 -hmac '<secret>' -hex
    //
    // payload="0."                              secret=""           => REF_EMPTY
    // payload='1714060800.{"a":1}'              secret="secret"     => REF_ASCII
    // payload='1714060800.héllo'                secret="secret"     => REF_UTF8
    // payload='1234567890.{"event":"webhook.test"}' secret="my-secret" => REF_DOC
    const REF_EMPTY: &str =
        "b849d5a581847b281957065739df36df2463d1977ea8d6e1e4e6cf33fadc68c3";
    const REF_ASCII: &str =
        "7aec304c817a19e63b9237165bbe9a1fd90c3d57a902d0982f9d8269804f0ff8";
    const REF_UTF8: &str =
        "98b3185b366f18f63455f9fe231f6f7c655c33aeacd7b5ad266e40a84e6f6345";
    const REF_DOC: &str =
        "60ac7756312e13849495558ecfc3d8d1a40c18fce095adaf12979dca9bff99c5";

    #[test]
    fn empty_body_and_empty_secret_matches_openssl() {
        assert_eq!(sign(0, "", ""), REF_EMPTY);
    }

    #[test]
    fn ascii_body_matches_openssl_reference() {
        assert_eq!(sign(1714060800, r#"{"a":1}"#, "secret"), REF_ASCII);
    }

    #[test]
    fn multibyte_utf8_body_matches_openssl_reference() {
        assert_eq!(sign(1714060800, "héllo", "secret"), REF_UTF8);
    }

    #[test]
    fn doc_reference_vector_matches_openssl() {
        assert_eq!(
            sign(1234567890, r#"{"event":"webhook.test"}"#, "my-secret"),
            REF_DOC
        );
    }

    #[test]
    fn output_is_64_lowercase_hex_chars() {
        let sig = sign(1714060800, "anything", "k");
        assert_eq!(sig.len(), 64, "SHA-256 hex digest must be 64 chars");
        assert!(
            sig.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex only, got: {sig}"
        );
    }

    #[test]
    fn different_timestamps_produce_different_signatures() {
        let a = sign(1, "body", "secret");
        let b = sign(2, "body", "secret");
        assert_ne!(a, b, "timestamp must affect signature (replay protection)");
    }

    #[test]
    fn different_secrets_produce_different_signatures() {
        let a = sign(1714060800, "body", "secret-a");
        let b = sign(1714060800, "body", "secret-b");
        assert_ne!(a, b, "secret must affect signature");
    }

    #[test]
    fn signing_is_deterministic() {
        let a = sign(1714060800, r#"{"k":"v"}"#, "s");
        let b = sign(1714060800, r#"{"k":"v"}"#, "s");
        assert_eq!(a, b, "same inputs must produce same signature");
    }

    #[test]
    fn signature_header_has_stripe_format() {
        let header = signature_header(1714060800, r#"{"a":1}"#, "secret");
        assert!(header.starts_with("t=1714060800,v1="), "got: {header}");
        assert!(header.ends_with(REF_ASCII), "header hex must match sign(): {header}");
    }
}
