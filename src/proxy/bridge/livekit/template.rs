//! `{var}` substitution for room/identity/metadata templates and the
//! webhook request body. Supported variables are caller-supplied — unknown
//! placeholders are a render-time error so misconfiguration surfaces
//! immediately rather than silently emitting half-rendered strings.

use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("template references unknown variable: {0}")]
    UnknownVariable(String),
    #[error("template has unterminated placeholder")]
    Unterminated,
}

/// Core substitution. `lookup` decides how each variable is rendered —
/// `render()` uses raw values, `render_for_json_string()` JSON-escapes
/// them so callers can drop the result safely inside a JSON string
/// context.
fn substitute_with<F>(template: &str, mut lookup: F) -> Result<String, TemplateError>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let close = after_open.find('}').ok_or(TemplateError::Unterminated)?;
        let var = &after_open[..close];
        let val = lookup(var).ok_or_else(|| TemplateError::UnknownVariable(var.to_string()))?;
        out.push_str(&val);
        rest = &after_open[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Render `template` with `{var}` placeholders replaced from `vars`.
/// Raw substitution — appropriate for plain-string outputs like room
/// names and participant identities. For outputs that will be parsed
/// as JSON, use [`render_for_json_string`].
pub fn render(template: &str, vars: &HashMap<&str, &str>) -> Result<String, TemplateError> {
    substitute_with(template, |var| vars.get(var).map(|s| s.to_string()))
}

/// Render `template` with each variable JSON-escaped before substitution.
/// Use this for templates whose rendered output is itself JSON (e.g. the
/// `metadata_template`, or a webhook `request_body_template` whose
/// `{var}` placeholders sit inside JSON string literals). Each var is
/// passed through `serde_json::to_string` and the surrounding quotes are
/// stripped — so `caller said "hi"` becomes `caller said \"hi\"`, safe
/// to splice into `"caller":"{from_user}"` without breaking JSON syntax.
pub fn render_for_json_string(
    template: &str,
    vars: &HashMap<&str, &str>,
) -> Result<String, TemplateError> {
    substitute_with(template, |var| {
        vars.get(var).map(|raw| {
            // serde_json::to_string of a &str always returns "..." — strip
            // the wrapping quotes so the caller's template can provide
            // them as part of the JSON string context.
            let quoted = serde_json::to_string(raw)
                .expect("serde_json::to_string of &str cannot fail");
            quoted[1..quoted.len() - 1].to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn render_substitutes_known_vars() {
        let v = vars(&[("from_user", "alice"), ("did", "12345")]);
        let out = render("caller-{from_user}-{did}", &v).unwrap();
        assert_eq!(out, "caller-alice-12345");
    }

    #[test]
    fn render_passes_through_text_with_no_placeholders() {
        let v = vars(&[]);
        let out = render("bot-fixed-name", &v).unwrap();
        assert_eq!(out, "bot-fixed-name");
    }

    #[test]
    fn render_empty_template() {
        let v = vars(&[]);
        let out = render("", &v).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    fn render_handles_adjacent_placeholders() {
        let v = vars(&[("a", "1"), ("b", "2")]);
        let out = render("{a}{b}", &v).unwrap();
        assert_eq!(out, "12");
    }

    #[test]
    fn render_rejects_unknown_var() {
        let v = vars(&[]);
        let err = render("{missing}", &v).unwrap_err();
        match err {
            TemplateError::UnknownVariable(name) => assert_eq!(name, "missing"),
            other => panic!("expected UnknownVariable, got {other:?}"),
        }
    }

    #[test]
    fn render_rejects_unterminated_placeholder() {
        let v = vars(&[]);
        let err = render("hello {oops", &v).unwrap_err();
        matches!(err, TemplateError::Unterminated);
    }

    #[test]
    fn render_unknown_var_in_middle_reports_correct_name() {
        let v = vars(&[("a", "x")]);
        let err = render("{a}-{missing}-{a}", &v).unwrap_err();
        match err {
            TemplateError::UnknownVariable(name) => assert_eq!(name, "missing"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn render_for_json_string_escapes_quotes() {
        // SIP user-part containing a quote — naive substitution would
        // produce invalid JSON. render_for_json_string must escape it.
        let v = vars(&[("from_user", "alice\"bob")]);
        let rendered = render_for_json_string("\"caller\":\"{from_user}\"", &v).unwrap();
        // The rendered string MUST parse as valid JSON when wrapped in {}.
        let full = format!("{{{rendered}}}");
        let parsed: serde_json::Value = serde_json::from_str(&full)
            .expect("JSON-safe render must produce parseable JSON");
        assert_eq!(parsed["caller"], "alice\"bob");
    }

    #[test]
    fn render_for_json_string_escapes_backslash() {
        let v = vars(&[("path", "c:\\users\\alice")]);
        let rendered = render_for_json_string("\"path\":\"{path}\"", &v).unwrap();
        let full = format!("{{{rendered}}}");
        let parsed: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert_eq!(parsed["path"], "c:\\users\\alice");
    }

    #[test]
    fn render_for_json_string_escapes_newline() {
        let v = vars(&[("note", "line1\nline2")]);
        let rendered = render_for_json_string("\"note\":\"{note}\"", &v).unwrap();
        let full = format!("{{{rendered}}}");
        let parsed: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert_eq!(parsed["note"], "line1\nline2");
    }

    #[test]
    fn render_for_json_string_passes_through_safe_values() {
        let v = vars(&[("from_user", "+919876543210")]);
        let rendered = render_for_json_string("\"caller\":\"{from_user}\"", &v).unwrap();
        assert_eq!(rendered, "\"caller\":\"+919876543210\"");
    }

    #[test]
    fn render_for_json_string_rejects_unknown_var() {
        let v = vars(&[]);
        let err = render_for_json_string("{missing}", &v).unwrap_err();
        match err {
            TemplateError::UnknownVariable(name) => assert_eq!(name, "missing"),
            other => panic!("got {other:?}"),
        }
    }
}
