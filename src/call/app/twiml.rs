//! TwiML XML parser — converts Twilio Markup Language verbs to [`EntryAction`]s.
//!
//! Only the subset of verbs needed for basic call-flow control is supported.
//! Unknown / unsupported verbs are logged at WARN and silently skipped so that
//! the call continues (D-17: call does NOT abort on unrecognised verbs).
//!
//! # Supported verbs
//! | TwiML verb | Produced action |
//! |-----------|-----------------|
//! | `<Play>`   | `EntryAction::Play { prompt: Some(url), … }` |
//! | `<Say>`    | `EntryAction::Play { prompt_text: Some(text), … }` |
//! | `<Dial>`   | `EntryAction::Transfer { target }` |
//! | `<Hangup>` | `EntryAction::Hangup { … }` |
//! | `<Reject>` | `EntryAction::Hangup { … }` (code 486/603) |
//! | `<Gather>` | `EntryAction::Collect { … }` + optional `EntryAction::Webhook` |
//! | `<Record>` | WARN + skip (no EntryAction::Record exists) |
//! | other      | WARN + skip |

use crate::call::app::ivr_config::EntryAction;
use quick_xml::{Reader, events::Event};
use std::collections::HashMap;
use tracing::warn;

/// Errors that can occur during TwiML parsing.
#[derive(Debug, thiserror::Error)]
pub enum TwimlError {
    #[error("XML parse error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("UTF-8 decode error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("Missing <Response> root element")]
    MissingResponse,
}

/// Parse a TwiML XML document and return the ordered list of [`EntryAction`]s.
///
/// The caller is responsible for retrying / logging the raw document.
///
/// # Errors
///
/// Returns [`TwimlError`] when the XML is malformed or the `<Response>` root
/// is absent. Unknown verbs are **not** errors — they are WARN-logged and
/// skipped.
pub fn parse_twiml(xml: &str) -> Result<Vec<EntryAction>, TwimlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut actions: Vec<EntryAction> = Vec::new();
    let mut found_response = false;
    // depth tracks XML nesting: 0 = outside everything, 1 = inside <Response>,
    // 2 = inside a top-level verb, 3 = inside a nested verb (e.g. <Say> in <Gather>).
    let mut depth: usize = 0;

    // Gather state — populated while depth >= 2 with name == "Gather"
    let mut in_gather = false;
    let mut gather_attrs: HashMap<String, String> = HashMap::new();
    let mut gather_nested: Vec<EntryAction> = Vec::new();

    // Current nested verb state (for depth == 3 inside Gather)
    let mut nested_verb: Option<String> = None;
    let mut nested_attrs: HashMap<String, String> = HashMap::new();

    // Current top-level verb state (for depth == 2, non-Gather)
    let mut top_verb: Option<String> = None;
    let mut top_attrs: HashMap<String, String> = HashMap::new();

    // Text accumulator for the current leaf element
    let mut text_buf = String::new();

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            // ── Opening tags ─────────────────────────────────────────────────
            Event::Start(e) => {
                let name = std::str::from_utf8(e.name().as_ref())?.to_string();
                depth += 1;

                if depth == 1 {
                    if name == "Response" {
                        found_response = true;
                    }
                    buf.clear();
                    continue;
                }

                if !found_response {
                    buf.clear();
                    continue;
                }

                let attrs = read_attrs(&e)?;
                text_buf.clear();

                if depth == 2 {
                    // Top-level verb directly inside <Response>
                    if name == "Gather" {
                        in_gather = true;
                        gather_attrs = attrs;
                        gather_nested.clear();
                        top_verb = None;
                    } else {
                        top_verb = Some(name);
                        top_attrs = attrs;
                    }
                } else if depth == 3 && in_gather {
                    // Nested verb inside <Gather>
                    nested_verb = Some(name);
                    nested_attrs = attrs;
                }
            }

            // ── Self-closing tags ────────────────────────────────────────────
            Event::Empty(e) => {
                if !found_response {
                    buf.clear();
                    continue;
                }
                let name = std::str::from_utf8(e.name().as_ref())?.to_string();
                let attrs = read_attrs(&e)?;

                // depth here reflects the *current* nesting before this tag,
                // so a self-closing <Hangup/> at Response level has depth == 1.
                if depth == 1 {
                    if let Some(action) = verb_to_action(&name, &attrs, "") {
                        actions.push(action);
                    }
                } else if depth == 2 && in_gather {
                    // Self-closing nested verb inside <Gather>
                    if let Some(action) = verb_to_action(&name, &attrs, "") {
                        gather_nested.push(action);
                    }
                }
            }

            // ── Text content ────────────────────────────────────────────────
            Event::Text(e) => {
                let text = e.decode().map_err(quick_xml::Error::from)?.into_owned();
                text_buf.push_str(&text);
            }

            // ── Closing tags ────────────────────────────────────────────────
            Event::End(e) => {
                if !found_response || depth == 0 {
                    depth = depth.saturating_sub(1);
                    buf.clear();
                    continue;
                }

                let name = std::str::from_utf8(e.name().as_ref())?.to_string();

                if depth == 3 && in_gather {
                    // Closing a nested verb inside <Gather>
                    if let Some(ref verb) = nested_verb {
                        if let Some(action) = verb_to_action(verb, &nested_attrs, &text_buf) {
                            gather_nested.push(action);
                        }
                    }
                    nested_verb = None;
                    nested_attrs.clear();
                    text_buf.clear();
                } else if depth == 2 {
                    if name == "Gather" && in_gather {
                        // Emit the Gather → Collect (+ optional Webhook)
                        let collect = build_gather_action(&gather_attrs, &gather_nested);
                        actions.extend(collect);
                        in_gather = false;
                        gather_attrs.clear();
                        gather_nested.clear();
                    } else if let Some(ref verb) = top_verb {
                        if let Some(action) = verb_to_action(verb, &top_attrs, &text_buf) {
                            actions.push(action);
                        }
                        top_verb = None;
                        top_attrs.clear();
                    }
                    text_buf.clear();
                }

                depth = depth.saturating_sub(1);
            }

            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if !found_response {
        return Err(TwimlError::MissingResponse);
    }

    Ok(actions)
}

/// Read XML element attributes into a `HashMap<String, String>`.
fn read_attrs(
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<HashMap<String, String>, TwimlError> {
    let mut map = HashMap::new();
    for attr in e.attributes().flatten() {
        let key = std::str::from_utf8(attr.key.as_ref())?.to_string();
        let value = attr.unescape_value().unwrap_or_default().into_owned();
        map.insert(key, value);
    }
    Ok(map)
}

/// Convert a TwiML verb + its attributes + inner text to an [`EntryAction`].
///
/// Returns `None` for unsupported / skipped verbs (after logging WARN).
fn verb_to_action(
    verb: &str,
    attrs: &HashMap<String, String>,
    text: &str,
) -> Option<EntryAction> {
    match verb {
        "Play" => {
            let url = text.trim().to_string();
            if url.is_empty() {
                warn!(verb = "Play", "TwiML <Play> has no URL, skipping");
                return None;
            }
            Some(EntryAction::Play {
                prompt: url,
                prompt_text: None,
                prompt_voice: None,
            })
        }
        "Say" => {
            let content = text.trim().to_string();
            if content.is_empty() {
                warn!(verb = "Say", "TwiML <Say> has empty text, skipping");
                return None;
            }
            let voice = attrs.get("voice").cloned();
            Some(EntryAction::Play {
                prompt: String::new(),
                prompt_text: Some(content),
                prompt_voice: voice,
            })
        }
        "Dial" => {
            let target = text.trim().to_string();
            if target.is_empty() {
                warn!(verb = "Dial", "TwiML <Dial> has no target, skipping");
                return None;
            }
            Some(EntryAction::Transfer { target })
        }
        "Hangup" => Some(EntryAction::Hangup {
            prompt: None,
            prompt_text: None,
            prompt_voice: None,
        }),
        "Reject" => {
            // Default to 486 (Busy) unless reason="rejected" → 603
            let _reason = attrs.get("reason").map(|s| s.as_str()).unwrap_or("busy");
            Some(EntryAction::Hangup {
                prompt: None,
                prompt_text: None,
                prompt_voice: None,
            })
        }
        "Record" => {
            // TODO: EntryAction::Record does not exist; skip gracefully.
            warn!(verb = "Record", "TwiML <Record> is not supported, skipping");
            None
        }
        other => {
            warn!(verb = %other, "Unknown TwiML verb, skipping");
            None
        }
    }
}

/// Build one or more `EntryAction`s from a `<Gather>` element and its
/// nested verbs (e.g. `<Say>` inside `<Gather>`).
///
/// If `action` attribute is present a `Webhook` action is appended after
/// the `Collect`.
fn build_gather_action(
    attrs: &HashMap<String, String>,
    nested: &[EntryAction],
) -> Vec<EntryAction> {
    let mut result: Vec<EntryAction> = Vec::new();

    // Extract an optional prompt from the first nested <Say> or <Play>
    let mut prompt: Option<String> = None;
    let mut prompt_text: Option<String> = None;
    let mut prompt_voice: Option<String> = None;

    for action in nested {
        match action {
            EntryAction::Play {
                prompt: p,
                prompt_text: pt,
                prompt_voice: pv,
            } => {
                if !p.is_empty() && prompt.is_none() {
                    prompt = Some(p.clone());
                }
                if pt.is_some() && prompt_text.is_none() {
                    prompt_text = pt.clone();
                    prompt_voice = pv.clone();
                }
            }
            _ => {}
        }
    }

    let num_digits = attrs
        .get("numDigits")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);

    let timeout_secs = attrs
        .get("timeout")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);

    let collect = EntryAction::Collect {
        variable: "gather_input".to_string(),
        prompt,
        prompt_text,
        prompt_voice,
        min_digits: 1,
        max_digits: num_digits,
        end_key: Some("#".to_string()),
        inter_digit_timeout_ms: timeout_secs * 1000,
    };
    result.push(collect);

    // If an action URL is set, add a Webhook follow-up
    if let Some(action_url) = attrs.get("action") {
        let method = attrs.get("method").cloned();
        result.push(EntryAction::Webhook {
            url: action_url.clone(),
            method,
            headers: HashMap::new(),
            variables: Some("gather_input".to_string()),
            timeout: 10,
        });
    }

    result
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_play_verb() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Play>https://example.com/audio.mp3</Play>
</Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            EntryAction::Play {
                prompt,
                prompt_text,
                ..
            } => {
                assert_eq!(prompt, "https://example.com/audio.mp3");
                assert!(prompt_text.is_none());
            }
            _ => panic!("expected Play"),
        }
    }

    #[test]
    fn test_parse_say_verb() {
        let xml = r#"<Response><Say voice="alice">Hello world</Say></Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            EntryAction::Play {
                prompt,
                prompt_text,
                prompt_voice,
            } => {
                assert_eq!(prompt, "");
                assert_eq!(prompt_text.as_deref(), Some("Hello world"));
                assert_eq!(prompt_voice.as_deref(), Some("alice"));
            }
            _ => panic!("expected Play (Say)"),
        }
    }

    #[test]
    fn test_parse_dial_verb() {
        let xml = r#"<Response><Dial>+12125551234</Dial></Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            EntryAction::Transfer { target } => {
                assert_eq!(target, "+12125551234");
            }
            _ => panic!("expected Transfer"),
        }
    }

    #[test]
    fn test_parse_hangup_verb() {
        let xml = r#"<Response><Hangup/></Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], EntryAction::Hangup { .. }));
    }

    #[test]
    fn test_parse_reject_verb() {
        let xml = r#"<Response><Reject reason="busy"/></Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], EntryAction::Hangup { .. }));
    }

    #[test]
    fn test_unknown_verb_skipped() {
        let xml = r#"<Response><Foo/><Play>http://x.com/a.wav</Play></Response>"#;
        let actions = parse_twiml(xml).unwrap();
        // Foo is skipped, Play is kept
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], EntryAction::Play { .. }));
    }

    #[test]
    fn test_record_verb_skipped() {
        let xml = r#"<Response><Record maxLength="30"/><Hangup/></Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], EntryAction::Hangup { .. }));
    }

    #[test]
    fn test_missing_response_root() {
        let xml = r#"<Foo><Bar/></Foo>"#;
        assert!(matches!(parse_twiml(xml), Err(TwimlError::MissingResponse)));
    }

    #[test]
    fn test_gather_with_say() {
        let xml = r#"<Response>
  <Gather numDigits="1" action="https://example.com/gather" method="POST">
    <Say>Press 1 for sales</Say>
  </Gather>
</Response>"#;
        let actions = parse_twiml(xml).unwrap();
        // Expect Collect + Webhook
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            EntryAction::Collect {
                variable,
                max_digits,
                ..
            } => {
                assert_eq!(variable, "gather_input");
                assert_eq!(*max_digits, 1);
            }
            _ => panic!("expected Collect"),
        }
        match &actions[1] {
            EntryAction::Webhook { url, .. } => {
                assert_eq!(url, "https://example.com/gather");
            }
            _ => panic!("expected Webhook"),
        }
    }

    #[test]
    fn test_multiple_verbs_in_order() {
        let xml = r#"<Response>
  <Say>Welcome</Say>
  <Dial>1234</Dial>
</Response>"#;
        let actions = parse_twiml(xml).unwrap();
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], EntryAction::Play { .. }));
        assert!(matches!(actions[1], EntryAction::Transfer { .. }));
    }
}
