//! E.164 normalization + international classification for CDR analytics
//! (analytics-console-page).
//!
//! "International" is deployment-relative: a call is international when its
//! destination is a full E.164 number whose country code differs from the
//! configured home dial code (default `+91`). National-format numbers (leading
//! `0`) and home-country numbers are domestic. Anything we can't confidently
//! place is treated as domestic — never silently counted as international.

/// Normalize a dialed number toward a comparable form. Strips separators; keeps
/// a leading `+`, maps a leading `00` (IDD prefix) to `+`, and leaves a national
/// leading `0` as-is. Returns `None` when there are no digits.
pub fn normalize_e164(number: &str) -> Option<String> {
    let mut digits = String::new();
    let mut plus = false;
    for (i, ch) in number.trim().chars().enumerate() {
        match ch {
            '+' if i == 0 => plus = true,
            c if c.is_ascii_digit() => digits.push(c),
            // ignore spaces, dashes, dots, parens, etc.
            _ => {}
        }
    }
    if digits.is_empty() {
        return None;
    }
    if plus {
        return Some(format!("+{digits}"));
    }
    // Leading "00" is the international dialing prefix → treat as '+'.
    if let Some(rest) = digits.strip_prefix("00") {
        if !rest.is_empty() {
            return Some(format!("+{rest}"));
        }
    }
    Some(digits)
}

/// Digits-only country code from a home dial code like `+91` or `91` → `"91"`.
fn home_cc(home_dial_code: &str) -> String {
    home_dial_code
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

/// True when `to_number` dials a country other than the home country.
///
/// `+<home>…` and national (`0…`/bare) numbers are domestic; `+<other>…` (or a
/// `00<other>` IDD form, normalized to `+`) is international. Unparseable numbers
/// are domestic (conservative — the international breakdown never over-counts).
pub fn is_international(to_number: &str, home_dial_code: &str) -> bool {
    let home = home_cc(home_dial_code);
    if home.is_empty() {
        return false; // no home country configured → don't classify
    }
    match normalize_e164(to_number) {
        Some(n) if n.starts_with('+') => !n[1..].starts_with(&home),
        // national / bare / unparseable → treated as home country
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_shapes() {
        assert_eq!(normalize_e164("+91 98123-45678").as_deref(), Some("+919812345678"));
        assert_eq!(normalize_e164("08035738267").as_deref(), Some("08035738267"));
        // 00 = IDD prefix → '+'; 0091… normalizes to +91… (dialing India)
        assert_eq!(normalize_e164("0091415550123").as_deref(), Some("+91415550123"));
        assert_eq!(normalize_e164("  "), None);
        assert_eq!(normalize_e164("(022) 2271-2701").as_deref(), Some("02222712701"));
    }

    #[test]
    fn international_relative_to_home() {
        // home +91
        assert!(!is_international("+919812345678", "+91")); // domestic
        assert!(!is_international("08035738267", "+91")); // national → domestic
        assert!(!is_international("02222712701", "+91")); // national → domestic
        assert!(is_international("+14155550123", "+91")); // US → international
        assert!(is_international("+442071838750", "+91")); // UK → international
        assert!(is_international("0044207183875", "+91")); // 00-IDD UK → international
        // unparseable / empty → domestic (never over-count)
        assert!(!is_international("", "+91"));
        assert!(!is_international("anonymous", "+91"));
    }

    #[test]
    fn home_code_accepts_with_or_without_plus() {
        assert!(is_international("+14155550123", "91"));
        assert!(!is_international("+919812345678", "91"));
        // no home configured → nothing is international
        assert!(!is_international("+14155550123", ""));
    }
}
