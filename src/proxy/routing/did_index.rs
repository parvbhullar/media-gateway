use crate::models::did;
use anyhow::Result;
use sea_orm::DatabaseConnection;
use std::collections::HashMap;
use std::sync::Arc;

/// Runtime view of a single DID row (only fields the matcher needs).
#[derive(Debug, Clone)]
pub struct DidEntry {
    pub number: String,
    pub trunk_name: Option<String>,
    pub extension_number: Option<String>,
    pub failover_trunk: Option<String>,
    /// `false` means the row exists but the operator has soft-disabled the
    /// number. The matcher uses this to actively `Reject` calls instead of
    /// silently falling through to rule-based routing.
    pub enabled: bool,
}

/// Immutable DID snapshot. Rebuilt on every refresh.
#[derive(Debug, Default)]
pub struct DidIndex {
    by_number: HashMap<String, DidEntry>,
}

impl DidIndex {
    pub async fn load(db: &DatabaseConnection) -> Result<Arc<Self>> {
        let rows = did::Model::list_all(db).await?;
        let mut by_number = HashMap::with_capacity(rows.len());
        for row in rows {
            // Keep disabled rows in the index — the matcher needs to see
            // them to actively reject calls with 403 "Number is disabled".
            // Previously these were filtered out which caused fall-through
            // to rule-based routing and calls would still complete.
            let enabled = row.enabled;
            by_number.insert(
                row.number.clone(),
                DidEntry {
                    number: row.number,
                    trunk_name: row.trunk_name,
                    extension_number: row.extension_number,
                    failover_trunk: row.failover_trunk,
                    enabled,
                },
            );
        }
        Ok(Arc::new(Self { by_number }))
    }

    pub fn lookup(&self, number: &str) -> Option<&DidEntry> {
        self.by_number.get(number)
    }

    pub fn len(&self) -> usize {
        self.by_number.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_number.is_empty()
    }
}

impl DidIndex {
    /// Construct a `DidIndex` from a raw map. Exposed for integration tests
    /// and call-path unit tests that need to synthesize a snapshot without
    /// touching the database.
    pub fn from_map_for_test(by_number: HashMap<String, DidEntry>) -> Self {
        Self { by_number }
    }
}
