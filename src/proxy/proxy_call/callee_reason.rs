//! Extract the callee/carrier failure reason from a SIP response so it reaches
//! the CDR, instead of recording only the status code.

/// Return the trimmed RFC 3326 `Reason:` header value (the carrier's detailed
/// cause, e.g. `Q.850;cause=17;text="user busy"`) when present and non-empty;
/// `None` preserves the prior behaviour of recording only the status code.
pub(crate) fn callee_reason_text(reason_header: Option<String>) -> Option<String> {
    reason_header
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_trimmed_reason_when_present() {
        assert_eq!(
            callee_reason_text(Some(
                "  Q.850;cause=17;text=\"user busy\"  ".to_string()
            )),
            Some("Q.850;cause=17;text=\"user busy\"".to_string())
        );
    }

    #[test]
    fn none_when_absent() {
        assert_eq!(callee_reason_text(None), None);
    }

    #[test]
    fn none_when_blank() {
        assert_eq!(callee_reason_text(Some("   ".to_string())), None);
    }
}
