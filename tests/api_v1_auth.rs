use rustpbx::handler::api_v1::auth::{API_KEY_PREFIX, issue_api_key, verify_api_key_hash};

#[test]
fn issue_generates_token_with_prefix_and_matching_hash() {
    let issued = issue_api_key();
    assert!(issued.plaintext.starts_with(API_KEY_PREFIX));
    // 5-char prefix ("rpbx_") + 64 hex chars
    assert_eq!(issued.plaintext.len(), API_KEY_PREFIX.len() + 64);
    assert!(verify_api_key_hash(&issued.plaintext, &issued.hash));
    assert!(!verify_api_key_hash("rpbx_deadbeef", &issued.hash));
}

#[test]
fn two_issued_keys_have_distinct_hashes() {
    let a = issue_api_key();
    let b = issue_api_key();
    assert_ne!(a.plaintext, b.plaintext);
    assert_ne!(a.hash, b.hash);
}
