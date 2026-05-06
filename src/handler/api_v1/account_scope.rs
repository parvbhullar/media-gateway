//! Per-request tenant scope injected by Bearer-auth middleware (Phase 13 — TEN-03).
//!
//! `AccountScope` is inserted into Axum's request extensions by
//! `api_v1_auth_middleware` and extracted by handlers that need to filter
//! resources to the calling tenant.  The `'root'` account is the master
//! tenant; all other slugs are sub-accounts.

use crate::handler::api_v1::common::CommonScopeQuery;
use crate::handler::api_v1::error::{ApiError, ApiResult};

pub const MASTER_ACCOUNT_ID: &str = "root";

/// Tenant scope resolved from the `api_keys.account_id` column.
#[derive(Debug, Clone)]
pub struct AccountScope {
    pub account_id: String,
    pub is_master: bool,
}

impl AccountScope {
    pub fn from_account_id(account_id: String) -> Self {
        let is_master = account_id == MASTER_ACCOUNT_ID;
        Self { account_id, is_master }
    }

    /// Returns `Err(forbidden_cross_account)` when a sub-account attempts to
    /// use `?account_id=` or `?include=all` to widen its scope.
    pub fn check_query_access(&self, q: &CommonScopeQuery) -> ApiResult<()> {
        if self.is_master {
            return Ok(());
        }
        if q.account_id.is_some() || q.include.as_deref() == Some("all") {
            return Err(ApiError::forbidden("forbidden_cross_account"));
        }
        Ok(())
    }

    /// The `account_id` value that should be used in WHERE clauses.
    ///
    /// Returns `None` only when the master uses `?include=all` — the caller
    /// should apply no account_id filter in that case.
    pub fn effective_filter(&self, q: &CommonScopeQuery) -> Option<String> {
        if self.is_master {
            if q.include.as_deref() == Some("all") {
                return None;
            }
            if let Some(a) = &q.account_id {
                return Some(a.clone());
            }
            return Some(MASTER_ACCOUNT_ID.to_string());
        }
        Some(self.account_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(account_id: Option<&str>, include: Option<&str>) -> CommonScopeQuery {
        CommonScopeQuery {
            account_id: account_id.map(str::to_string),
            include: include.map(str::to_string),
        }
    }

    #[test]
    fn master_scope_is_identified_correctly() {
        let s = AccountScope::from_account_id("root".into());
        assert!(s.is_master);
    }

    #[test]
    fn sub_account_scope_is_not_master() {
        let s = AccountScope::from_account_id("acme".into());
        assert!(!s.is_master);
    }

    #[test]
    fn master_default_filter_is_root() {
        let s = AccountScope::from_account_id("root".into());
        assert_eq!(s.effective_filter(&q(None, None)), Some("root".into()));
    }

    #[test]
    fn master_with_include_all_returns_none() {
        let s = AccountScope::from_account_id("root".into());
        assert_eq!(s.effective_filter(&q(None, Some("all"))), None);
    }

    #[test]
    fn master_with_account_id_scopes_to_that_account() {
        let s = AccountScope::from_account_id("root".into());
        assert_eq!(s.effective_filter(&q(Some("acme"), None)), Some("acme".into()));
    }

    #[test]
    fn sub_account_always_filters_to_own_id() {
        let s = AccountScope::from_account_id("acme".into());
        assert_eq!(s.effective_filter(&q(None, None)), Some("acme".into()));
    }

    #[test]
    fn sub_account_using_include_all_gets_forbidden() {
        let s = AccountScope::from_account_id("acme".into());
        assert!(s.check_query_access(&q(None, Some("all"))).is_err());
    }

    #[test]
    fn sub_account_using_account_id_gets_forbidden() {
        let s = AccountScope::from_account_id("acme".into());
        assert!(s.check_query_access(&q(Some("other"), None)).is_err());
    }

    #[test]
    fn master_check_always_passes() {
        let s = AccountScope::from_account_id("root".into());
        assert!(s.check_query_access(&q(Some("any"), Some("all"))).is_ok());
    }
}
