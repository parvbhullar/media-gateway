//! Phase 5 Plan 05-04 Task 4 (sub-task 4a) — per-trunk ACL evaluator.
//!
//! Evaluates `supersip_trunk_acl_entries` rules at INVITE time against the
//! peer's `IpAddr`. Grammar matches `validate_acl_rule` (D-13):
//!   `^(allow|deny) (all|<IP>|<CIDR>)$`
//!
//! Rules are scanned top-to-bottom; first match wins. Default verdict is
//! Allow (D-14).

use ipnetwork::IpNetwork;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclVerdict {
    Allow,
    Deny,
}

enum Action {
    Allow,
    Deny,
}

enum Target {
    All,
    Ip(IpAddr),
    Cidr(IpNetwork),
}

fn parse_rule(rule: &str) -> Option<(Action, Target)> {
    let trimmed = rule.trim();
    let mut parts = trimmed.splitn(2, ' ');
    let action = parts.next()?.to_ascii_lowercase();
    let target = parts.next()?.trim();

    let action = match action.as_str() {
        "allow" => Action::Allow,
        "deny" => Action::Deny,
        _ => return None,
    };

    let target = if target.eq_ignore_ascii_case("all") {
        Target::All
    } else if target.contains('/') {
        match target.parse::<IpNetwork>() {
            Ok(n) => Target::Cidr(n),
            Err(_) => return None,
        }
    } else {
        match target.parse::<IpAddr>() {
            Ok(ip) => Target::Ip(ip),
            Err(_) => return None,
        }
    };

    Some((action, target))
}

fn target_matches(target: &Target, ip: IpAddr) -> bool {
    match target {
        Target::All => true,
        Target::Ip(t) => *t == ip,
        Target::Cidr(net) => net.contains(ip),
    }
}

/// Evaluate rules against `peer_ip`. Top-to-bottom, first match wins,
/// default = Allow (D-14).
pub fn evaluate_acl_rules(rules: &[String], peer_ip: IpAddr) -> AclVerdict {
    for rule in rules {
        if let Some((action, target)) = parse_rule(rule) {
            if target_matches(&target, peer_ip) {
                return match action {
                    Action::Allow => AclVerdict::Allow,
                    Action::Deny => AclVerdict::Deny,
                };
            }
        }
        // Malformed rules silently skipped — handler validates on insert.
    }
    AclVerdict::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn acl_eval_no_rules_default_allow() {
        let rules: Vec<String> = vec![];
        assert_eq!(
            evaluate_acl_rules(&rules, "1.2.3.4".parse().unwrap()),
            AclVerdict::Allow
        );
    }

    #[test]
    fn acl_eval_first_match_wins() {
        let rules = s(&["deny 1.2.3.4", "allow all"]);
        assert_eq!(
            evaluate_acl_rules(&rules, "1.2.3.4".parse().unwrap()),
            AclVerdict::Deny
        );
    }

    #[test]
    fn acl_eval_default_allow_when_no_match() {
        let rules = s(&["deny 9.9.9.9"]);
        assert_eq!(
            evaluate_acl_rules(&rules, "1.2.3.4".parse().unwrap()),
            AclVerdict::Allow
        );
    }

    #[test]
    fn acl_eval_cidr_match() {
        let rules = s(&["allow 10.0.0.0/8"]);
        assert_eq!(
            evaluate_acl_rules(&rules, "10.5.5.5".parse().unwrap()),
            AclVerdict::Allow
        );
    }

    #[test]
    fn acl_eval_ipv6_cidr_match() {
        let rules = s(&["deny 2001:db8::/32"]);
        assert_eq!(
            evaluate_acl_rules(&rules, "2001:db8::1".parse().unwrap()),
            AclVerdict::Deny
        );
    }

    #[test]
    fn acl_eval_allow_all_terminal() {
        let rules = s(&["allow all"]);
        assert_eq!(
            evaluate_acl_rules(&rules, "1.2.3.4".parse().unwrap()),
            AclVerdict::Allow
        );
        assert_eq!(
            evaluate_acl_rules(&rules, "203.0.113.5".parse().unwrap()),
            AclVerdict::Allow
        );
    }
}
