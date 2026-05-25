//! Thin wrapper over `livekit_api::access_token::AccessToken`.
//! Kept separate so unit tests can assert claims without pulling in the
//! full dispatcher.

use std::time::Duration;

use anyhow::{Result, anyhow};
use livekit_api::access_token::{AccessToken, VideoGrants};

pub struct MintInput<'a> {
    pub api_key: &'a str,
    pub api_secret: &'a str,
    pub identity: &'a str,
    pub room: &'a str,
    pub metadata: Option<&'a str>,
    pub ttl_secs: u64,
}

/// Mint a LiveKit join JWT with grants suitable for a SIP→LiveKit caller
/// participant: room_join + room_create + can_publish + can_subscribe.
pub fn mint(input: MintInput<'_>) -> Result<String> {
    if input.api_key.trim().is_empty() {
        return Err(anyhow!("api_key must not be empty"));
    }
    if input.api_secret.trim().is_empty() {
        return Err(anyhow!("api_secret must not be empty"));
    }
    let mut tok = AccessToken::with_api_key(input.api_key, input.api_secret)
        .with_identity(input.identity)
        .with_grants(VideoGrants {
            room_join: true,
            room_create: true,
            can_publish: true,
            can_subscribe: true,
            room: input.room.to_string(),
            ..Default::default()
        })
        .with_ttl(Duration::from_secs(input.ttl_secs));
    if let Some(md) = input.metadata {
        tok = tok.with_metadata(md);
    }
    tok.to_jwt()
        .map_err(|e| anyhow!("livekit JWT mint failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn decode_payload(jwt: &str) -> serde_json::Value {
        let payload_b64 = jwt.split('.').nth(1).expect("jwt has 3 segments");
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .expect("jwt payload is base64url");
        serde_json::from_slice(&bytes).expect("payload is JSON")
    }

    fn good_input<'a>() -> MintInput<'a> {
        MintInput {
            api_key:    "APItestkey",
            api_secret: "averylongsecretvalueofatleast32chars",
            identity:   "alice",
            room:       "demo-room",
            metadata:   None,
            ttl_secs:   60,
        }
    }

    #[test]
    fn mint_emits_three_segment_jwt() {
        let jwt = mint(good_input()).expect("mint OK");
        assert_eq!(jwt.split('.').count(), 3, "JWT must be header.payload.signature");
    }

    #[test]
    fn mint_payload_includes_identity_and_room() {
        let jwt = mint(good_input()).expect("mint OK");
        let v = decode_payload(&jwt);
        // Standard JWT subject claim
        assert_eq!(v.get("sub").and_then(|x| x.as_str()), Some("alice"));
        // VideoGrants nested under "video"
        let video = v.get("video").expect("video grants present");
        assert_eq!(video.get("room").and_then(|x| x.as_str()), Some("demo-room"));
        assert_eq!(video.get("roomJoin").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(video.get("roomCreate").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(video.get("canPublish").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(video.get("canSubscribe").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn mint_with_metadata_includes_it_in_payload() {
        let mut input = good_input();
        let md = r#"{"tenant":"acme"}"#;
        input.metadata = Some(md);
        let jwt = mint(input).expect("mint OK");
        let v = decode_payload(&jwt);
        assert_eq!(v.get("metadata").and_then(|x| x.as_str()), Some(md));
    }

    #[test]
    fn mint_without_metadata_omits_field_or_emits_empty_string() {
        let jwt = mint(good_input()).expect("mint OK");
        let v = decode_payload(&jwt);
        // Either the field is absent OR is an empty string — both are acceptable
        // behaviours of livekit_api. We only assert it's not a non-empty string.
        match v.get("metadata") {
            None => {}
            Some(serde_json::Value::String(s)) => assert!(s.is_empty(), "got metadata={s}"),
            other => panic!("unexpected metadata shape: {other:?}"),
        }
    }

    #[test]
    fn mint_rejects_empty_api_key() {
        let mut input = good_input();
        input.api_key = "";
        let err = mint(input).unwrap_err();
        assert!(err.to_string().contains("api_key"));
    }

    #[test]
    fn mint_rejects_empty_api_secret() {
        let mut input = good_input();
        input.api_secret = "";
        let err = mint(input).unwrap_err();
        assert!(err.to_string().contains("api_secret"));
    }

    #[test]
    fn mint_ttl_appears_in_exp_claim() {
        // livekit_api emits `exp` but not always `iat` — we just assert
        // exp is within roughly [now, now+ttl+slop] seconds.
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let jwt = mint(good_input()).expect("mint OK");
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let v = decode_payload(&jwt);
        let exp = v.get("exp").and_then(|x| x.as_i64()).expect("exp claim");
        // ttl_secs = 60 in good_input — exp should be roughly now+60.
        assert!(
            exp >= before + 60 && exp <= after + 60 + 1,
            "exp={exp} not in [before+60={}, after+61={}] (ttl_secs=60)",
            before + 60,
            after + 61
        );
    }
}
