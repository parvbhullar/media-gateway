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

/// Render `template` with `{var}` placeholders replaced from `vars`.
/// Each `{...}` segment is looked up in `vars`; an unknown variable is a
/// render-time error.
pub fn render(template: &str, vars: &HashMap<&str, &str>) -> Result<String, TemplateError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let close = after_open
            .find('}')
            .ok_or(TemplateError::Unterminated)?;
        let var = &after_open[..close];
        let val = vars
            .get(var)
            .ok_or_else(|| TemplateError::UnknownVariable(var.to_string()))?;
        out.push_str(val);
        rest = &after_open[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
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
}
