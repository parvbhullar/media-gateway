//! Shared primitives for the `/api/v1/*` REST surface (Phase 1, Plan 01).
//!
//! Every list endpoint in v2.0 uses the [`Pagination`] query extractor and
//! wraps results in [`PaginatedResponse`]. The envelope shape is locked in
//! `.planning/phases/01-api-shell-cheap-wrappers/01-CONTEXT.md` §"Pagination
//! envelope (SHELL-02)" and must not drift.

use sea_orm::{ColumnTrait, Condition};
use serde::{Deserialize, Serialize};

fn default_page() -> u64 {
    1
}

fn default_page_size() -> u64 {
    20
}

const MAX_PAGE_SIZE: u64 = 200;

/// Query extractor for `?page=&page_size=`.
///
/// Defaults: `page = 1`, `page_size = 20`. `page_size` is clamped to a
/// hard ceiling of 200 in [`Pagination::limit`] to prevent pathological
/// request sizes from downstream handlers.
#[derive(Debug, Clone, Deserialize)]
pub struct Pagination {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_page_size")]
    pub page_size: u64,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            page: default_page(),
            page_size: default_page_size(),
        }
    }
}

impl Pagination {
    /// SQL offset derived from the current page.
    ///
    /// `saturating_sub` guards against `page = 0` (the `Deserialize` impl
    /// accepts arbitrary values; the handler decides whether to clamp or
    /// reject).
    pub fn offset(&self) -> u64 {
        self.page.saturating_sub(1) * self.limit()
    }

    /// Effective page size, clamped to [`MAX_PAGE_SIZE`].
    pub fn limit(&self) -> u64 {
        self.page_size.min(MAX_PAGE_SIZE).max(1)
    }
}

/// Response envelope for paginated list endpoints.
///
/// Shape (locked): `{items, page, page_size, total}`.
#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub items: Vec<T>,
    pub page: u64,
    pub page_size: u64,
    pub total: u64,
}

impl<T: Serialize> PaginatedResponse<T> {
    pub fn new(items: Vec<T>, page: u64, page_size: u64, total: u64) -> Self {
        Self {
            items,
            page,
            page_size,
            total,
        }
    }
}

/// Query parameters for tenant scope: `?account_id=<slug>` and `?include=all`.
///
/// Accepted by every list endpoint after the handler retrofit in 13-01d.
#[derive(Debug, Default, Deserialize)]
pub struct CommonScopeQuery {
    pub account_id: Option<String>,
    pub include: Option<String>,
}

/// Build the tenant `Condition` fragment for a list query.
///
/// Calls `scope.check_query_access` (returns 403 for scope violations) then
/// applies the appropriate `account_id` equality filter. Returns the base
/// `Condition` unmodified when the master uses `?include=all`.
pub(crate) fn build_account_filter<C: ColumnTrait>(
    scope: &crate::handler::api_v1::account_scope::AccountScope,
    column: C,
    q: &CommonScopeQuery,
    base: Condition,
) -> crate::handler::api_v1::error::ApiResult<Condition> {
    scope.check_query_access(q)?;
    Ok(match scope.effective_filter(q) {
        Some(acct) => base.add(column.eq(acct)),
        None => base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_default_is_page_one_size_twenty() {
        let p = Pagination::default();
        assert_eq!(p.page, 1);
        assert_eq!(p.page_size, 20);
        assert_eq!(p.offset(), 0);
        assert_eq!(p.limit(), 20);
    }

    #[test]
    fn pagination_offset_for_page_three_size_fifty() {
        let p = Pagination {
            page: 3,
            page_size: 50,
        };
        assert_eq!(p.offset(), 100);
        assert_eq!(p.limit(), 50);
    }

    #[test]
    fn pagination_limit_clamped_to_max() {
        let p = Pagination {
            page: 1,
            page_size: 10_000,
        };
        assert_eq!(p.limit(), MAX_PAGE_SIZE);
    }

    #[test]
    fn pagination_zero_page_size_clamps_to_one() {
        let p = Pagination {
            page: 1,
            page_size: 0,
        };
        assert_eq!(p.limit(), 1);
    }

    #[test]
    fn paginated_response_serializes_locked_shape() {
        let r = PaginatedResponse::new(vec![1u32, 2, 3], 1, 20, 3);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["items"], serde_json::json!([1, 2, 3]));
        assert_eq!(v["page"], 1);
        assert_eq!(v["page_size"], 20);
        assert_eq!(v["total"], 3);
    }
}
