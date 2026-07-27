use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use glob::glob;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::ErrorKind,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use tracing::{info, warn};

use crate::{
    addons::queue::services::utils as queue_utils,
    config::{ProxyConfig, RecordingPolicy},
    models::{kind_schemas, routing, sip_trunk, trunk::SipTrunkConfig},
    proxy::routing::matcher::RouteResourceLookup,
    proxy::routing::{
        CacPolicy, CallIdMode, ConfigOrigin, DestConfig, MatchConditions, MediaMode,
        RewriteRules, RouteAction, RouteDirection, RouteQueueConfig, RouteRule, TrunkConfig,
        VideoPolicy,
        did_index::DidIndex,
    },
    proxy::trunk_registrar::TrunkRegistrar,
};

pub struct ProxyDataContext {
    config: RwLock<Arc<ProxyConfig>>,
    trunks: RwLock<HashMap<String, TrunkConfig>>,
    queues: RwLock<HashMap<String, RouteQueueConfig>>,
    routes: RwLock<Vec<RouteRule>>,
    acl_rules: RwLock<Vec<String>>,
    did_index: RwLock<Arc<DidIndex>>,
    did_default_country: RwLock<Option<String>>,
    did_strict_mode: RwLock<bool>,
    db: Option<DatabaseConnection>,
    trunk_registrar: Arc<TrunkRegistrar>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReloadMetrics {
    pub total: usize,
    pub config_count: usize,
    pub file_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<GeneratedFileMetrics>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedFileMetrics {
    pub entries: usize,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

impl ProxyDataContext {
    pub async fn new(config: Arc<ProxyConfig>, db: Option<DatabaseConnection>) -> Result<Self> {
        let trunk_registrar = Arc::new(TrunkRegistrar::new());

        let ctx = Self {
            config: RwLock::new(config.clone()),
            trunks: RwLock::new(HashMap::new()),
            queues: RwLock::new(HashMap::new()),
            routes: RwLock::new(Vec::new()),
            acl_rules: RwLock::new(Vec::new()),
            did_index: RwLock::new(Arc::new(DidIndex::default())),
            did_default_country: RwLock::new(None),
            did_strict_mode: RwLock::new(false),
            db,
            trunk_registrar,
        };
        let _ = ctx.reload_trunks(true, None).await?;
        let _ = ctx.reload_queues(false, None).await?;
        let _ = ctx.reload_routes(true, None).await?;
        let _ = ctx.reload_acl_rules(false, None)?;
        if let Some(db) = ctx.db.as_ref() {
            if let Err(e) = crate::models::backfill_dids_from_sip_trunks::run(db).await {
                warn!(error = %e, "DID backfill failed, continuing startup");
            }
        }
        ctx.reload_did_index().await;
        Ok(ctx)
    }

    pub fn did_index(&self) -> Arc<DidIndex> {
        self.did_index.read().unwrap().clone()
    }

    pub fn set_did_index(&self, idx: Arc<DidIndex>) {
        *self.did_index.write().unwrap() = idx;
    }

    /// Cached `routing.default_country` snapshot (uppercase ISO alpha-2).
    /// Refreshed alongside the DID index via [`reload_did_index`].
    pub fn did_default_country(&self) -> Option<String> {
        self.did_default_country.read().unwrap().clone()
    }

    /// Cached `routing.did_strict_mode` snapshot. Refreshed alongside the
    /// DID index via [`reload_did_index`].
    pub fn did_strict_mode(&self) -> bool {
        *self.did_strict_mode.read().unwrap()
    }

    fn set_did_default_country(&self, value: Option<String>) {
        *self.did_default_country.write().unwrap() = value;
    }

    fn set_did_strict_mode(&self, value: bool) {
        *self.did_strict_mode.write().unwrap() = value;
    }

    /// Rebuild the DID index from the database and refresh cached DID
    /// settings (`routing.default_country`, `routing.did_strict_mode`).
    /// Logs and installs an empty index / default settings on error so
    /// callers can always rely on `did_index()` returning a valid snapshot.
    pub async fn reload_did_index(&self) {
        let Some(db) = self.db.as_ref() else {
            self.set_did_index(Arc::new(DidIndex::default()));
            self.set_did_default_country(None);
            self.set_did_strict_mode(false);
            return;
        };
        match DidIndex::load(db).await {
            Ok(idx) => self.set_did_index(idx),
            Err(e) => {
                warn!(error = %e, "failed to load DID index; keeping empty index");
                self.set_did_index(Arc::new(DidIndex::default()));
            }
        }
        let default_country = crate::config_merge::read_default_country(db).await;
        let strict = crate::config_merge::read_did_strict_mode(db).await;
        self.set_did_default_country(default_country);
        self.set_did_strict_mode(strict);
    }

    pub fn trunk_registrar(&self) -> &Arc<TrunkRegistrar> {
        &self.trunk_registrar
    }

    pub fn config(&self) -> Arc<ProxyConfig> {
        self.config.read().unwrap().clone()
    }

    pub fn update_config(&self, config: Arc<ProxyConfig>) {
        *self.config.write().unwrap() = config;
    }

    pub fn trunks_snapshot(&self) -> HashMap<String, TrunkConfig> {
        self.trunks.read().unwrap().clone()
    }

    pub fn get_trunk(&self, name: &str) -> Option<TrunkConfig> {
        self.trunks.read().unwrap().get(name).cloned()
    }

    pub fn routes_snapshot(&self) -> Vec<RouteRule> {
        self.routes.read().unwrap().clone()
    }

    pub fn queues_snapshot(&self) -> HashMap<String, RouteQueueConfig> {
        self.queues.read().unwrap().clone()
    }

    pub fn acl_rules_snapshot(&self) -> Vec<String> {
        self.acl_rules.read().unwrap().clone()
    }

    pub fn resolve_queue_config(&self, reference: &str) -> Result<Option<RouteQueueConfig>> {
        if reference.trim().is_empty() {
            return Ok(None);
        }

        // Try to resolve by ID first (db-<id>)
        if let Some(id_str) = reference.strip_prefix("db-")
            && id_str.parse::<i64>().is_ok()
        {
            let queues = self.queues.read().unwrap();
            // We need to store the ID in the map key or value to look it up efficiently.
            // Currently keys are canonical names or "db-<id>" from queue_entry_key.
            // Let's check if the key exists directly.
            if let Some(queue) = queues.get(reference) {
                return Ok(Some(queue.clone()));
            }
        }

        // Try to resolve by file path
        if let Some(config) = self.load_queue_file(reference)? {
            return Ok(Some(config));
        }

        if reference.chars().all(|c| c.is_ascii_digit()) && !reference.is_empty() {
            let db_key = format!("db-{}", reference);
            return self.resolve_queue_config(&db_key);
        }

        let Some(key) = queue_utils::canonical_queue_key(reference) else {
            return Ok(None);
        };

        let queues = self.queues.read().unwrap();
        for (name, queue) in queues.iter() {
            if let Some(existing) = queue_utils::canonical_queue_key(name)
                && existing == key
            {
                return Ok(Some(queue.clone()));
            }
            // Also check the queue name inside the config, in case the key is an ID
            if let Some(queue_name) = &queue.name
                && let Some(existing) = queue_utils::canonical_queue_key(queue_name)
                && existing == key
            {
                return Ok(Some(queue.clone()));
            }
        }
        Ok(None)
    }

    pub fn load_queue_file(&self, reference: &str) -> Result<Option<RouteQueueConfig>> {
        let trimmed = reference.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let config = self.config.read().unwrap().clone();
        let base = config.generated_queue_dir();
        let path = Self::resolve_reference_path(base.as_path(), trimmed);
        Self::read_queue_document(path)
    }

    fn resolve_reference_path(base: &Path, reference: &str) -> PathBuf {
        let candidate = Path::new(reference);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            base.join(candidate)
        }
    }

    fn read_queue_document(path: PathBuf) -> Result<Option<RouteQueueConfig>> {
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let doc: queue_utils::QueueFileDocument = toml::from_str(&contents)
                    .with_context(|| format!("failed to parse queue file {}", path.display()))?;
                Ok(Some(doc.queue))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(err).with_context(|| format!("failed to read queue file {}", path.display()))
            }
        }
    }

    pub async fn find_trunk_by_ip(&self, addr: &IpAddr) -> Option<String> {
        let trunks = self.trunks_snapshot();
        for (name, trunk) in trunks.iter() {
            if trunk.matches_inbound_ip(addr).await {
                return Some(name.clone());
            }
        }
        None
    }

    pub async fn find_trunks_by_ip(&self, addr: &IpAddr) -> Vec<String> {
        let trunks = self.trunks_snapshot();
        let mut matches = Vec::new();
        for (name, trunk) in trunks.iter() {
            if trunk.matches_inbound_ip(addr).await {
                matches.push(name.clone());
            }
        }
        matches
    }

    pub async fn reload_trunks(
        &self,
        generated_toml: bool,
        config_override: Option<Arc<ProxyConfig>>,
    ) -> Result<ReloadMetrics> {
        if let Some(config) = config_override {
            *self.config.write().unwrap() = config;
        }

        let config = self.config.read().unwrap().clone();

        let started_at = Utc::now();
        let default_dir = config.generated_trunks_dir();
        let mut generated_entries = 0usize;
        let generated = if generated_toml {
            self.export_trunks_to_toml(&config, default_dir.as_path())
                .await?
        } else {
            None
        };
        if let Some(ref info) = generated {
            generated_entries = info.entries;
        }
        let mut trunks: HashMap<String, TrunkConfig> = HashMap::new();
        let mut config_count = 0usize;
        let mut file_count = 0usize;
        let mut files: Vec<String> = Vec::new();
        let patterns = config.trunks_files.clone();
        if !config.trunks.is_empty() {
            config_count = config.trunks.len();
            info!(count = config_count, "loading trunks from embedded config");
            for (name, trunk) in config.trunks.iter() {
                let mut copy = trunk.clone();
                copy.origin = ConfigOrigin::embedded();
                trunks.insert(name.clone(), copy);
            }
        }
        if !config.trunks_files.is_empty() {
            let (file_trunks, file_paths) = load_trunks_from_files(&config.trunks_files)?;
            file_count = file_trunks.len();
            if !file_paths.is_empty() {
                files.extend(file_paths);
            }
            trunks.extend(file_trunks);
        }
        if let Some(ref info) = generated {
            let generated_pattern = vec![info.path.clone()];
            let (generated_trunks, _) = load_trunks_from_files(&generated_pattern)?;
            trunks.extend(generated_trunks);
        } else {
            // When not regenerating (no DB or generated_toml=false), load from any
            // previously-generated file on disk so restarts pick up DB-managed trunks.
            let generated_path = config
                .generated_trunks_dir()
                .join("trunks.generated.toml");
            if generated_path.exists() {
                let pattern = vec![generated_path.to_string_lossy().to_string()];
                let (generated_trunks, generated_files) = load_trunks_from_files(&pattern)?;
                file_count += generated_trunks.len();
                files.extend(generated_files);
                trunks.extend(generated_trunks);
            }
        }

        let len = trunks.len();
        *self.trunks.write().unwrap() = trunks.clone();

        let acl_enabled = config
            .modules
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .any(|m| m == "acl");
        if !acl_enabled && trunks.values().any(|t| !t.inbound_hosts.is_empty()) {
            warn!(
                "inbound_hosts is configured on one or more trunks but the 'acl' module is \
                 not listed in proxy.modules — inbound IP filtering will be silently skipped. \
                 Add 'acl' to proxy.modules to enable it."
            );
        }

        // Reconcile trunk registrations after reload.
        self.trunk_registrar.reconcile(&trunks).await;

        // Promote file-trunks that have no DB row yet so the health prober
        // (which iterates `rustpbx_trunks`) can see them. Insert-only:
        // existing rows (including live status columns) are never overwritten.
        if let Some(db) = self.db.as_ref() {
            let report = crate::proxy::file_trunk_sync::promote_missing_file_trunks(db, &trunks)
                .await;
            if report.inserted > 0 || report.skipped_invalid > 0 {
                info!(
                    considered = report.considered,
                    inserted = report.inserted,
                    skipped_existing = report.skipped_existing,
                    skipped_embedded = report.skipped_embedded,
                    skipped_generated = report.skipped_generated,
                    skipped_invalid = report.skipped_invalid,
                    "file_trunk_sync: promoted file trunks into rustpbx_trunks"
                );
            }
        }

        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds();
        info!(
            total = len,
            config_count, file_count, generated_entries, duration_ms, "trunks reloaded"
        );
        Ok(ReloadMetrics {
            total: len,
            config_count,
            file_count,
            generated,
            files,
            patterns,
            started_at,
            finished_at,
            duration_ms,
        })
    }

    pub async fn reload_queues(
        &self,
        _generated_toml: bool,
        config_override: Option<Arc<ProxyConfig>>,
    ) -> Result<ReloadMetrics> {
        if let Some(config) = config_override {
            *self.config.write().unwrap() = config;
        }

        let config = self.config.read().unwrap().clone();
        let started_at = Utc::now();

        let mut queues: HashMap<String, RouteQueueConfig> = HashMap::new();
        let mut config_count = 0usize;
        let mut file_count = 0usize;
        let mut files: Vec<String> = Vec::new();
        let patterns = config.queues_files.clone();

        if !config.queues.is_empty() {
            config_count = config.queues.len();
            info!(count = config_count, "loading queues from embedded config");
            for (name, mut queue) in config.queues.clone().into_iter() {
                queue.origin = ConfigOrigin::embedded();
                queues.insert(name, queue);
            }
        }

        if !config.queues_files.is_empty() {
            match queue_utils::load_queues_from_files(&config.queues_files) {
                Ok((file_queues, file_paths)) => {
                    file_count = file_queues.len();
                    if !file_paths.is_empty() {
                        files.extend(file_paths.clone());
                    }
                    for (key, mut queue) in file_queues {
                        let path = file_paths
                            .iter()
                            .find(|p| p.contains(&key))
                            .cloned()
                            .unwrap_or_else(|| config.queues_files.join(", "));
                        queue.origin = ConfigOrigin::from_file(path);
                        queues.insert(key, queue);
                    }
                }
                Err(e) => {
                    tracing::error!("failed to load queues from files: {}", e);
                }
            }
        }

        let generated_file = config.generated_queue_dir().join("queues.generated.toml");
        if generated_file.exists() {
            match fs::read_to_string(&generated_file) {
                Ok(content) => {
                    match toml::from_str::<HashMap<String, RouteQueueConfig>>(&content) {
                        Ok(loaded) => {
                            file_count += loaded.len();
                            files.push(generated_file.display().to_string());
                            queues.extend(loaded);
                        }
                        Err(e) => {
                            tracing::error!("failed to parse queues.generated.toml: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("failed to read queues.generated.toml: {}", e);
                }
            }
        }

        let len = queues.len();
        *self.queues.write().unwrap() = queues;
        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds();
        info!(
            total = len,
            config_count, file_count, duration_ms, "queues reloaded"
        );

        Ok(ReloadMetrics {
            total: len,
            config_count,
            file_count,
            generated: None,
            files,
            patterns,
            started_at,
            finished_at,
            duration_ms,
        })
    }

    pub async fn reload_routes(
        &self,
        generated_toml: bool,
        config_override: Option<Arc<ProxyConfig>>,
    ) -> Result<ReloadMetrics> {
        if let Some(config) = config_override {
            *self.config.write().unwrap() = config;
        }

        let config = self.config.read().unwrap().clone();

        let started_at = Utc::now();
        let default_dir = config.generated_routes_dir();
        let generated = if generated_toml {
            self.export_routes_to_toml(&config, default_dir.as_path())
                .await?
        } else {
            None
        };
        let generated_entries = if let Some(ref info) = generated {
            info.entries
        } else {
            0usize
        };
        let mut routes: Vec<RouteRule> = Vec::new();
        let mut config_count = 0usize;
        let mut file_count = 0usize;
        let mut files: Vec<String> = Vec::new();
        let patterns = config.routes_files.clone();
        if let Some(cfg_routes) = config.routes.clone() {
            config_count = cfg_routes.len();
            info!(count = config_count, "loading routes from embedded config");
            for mut route in cfg_routes {
                route.origin = ConfigOrigin::embedded();
                upsert_route(&mut routes, route);
            }
        }
        if !config.routes_files.is_empty() {
            let (file_routes, file_paths) = load_routes_from_files(&config.routes_files)?;
            file_count = file_routes.len();
            if !file_paths.is_empty() {
                files.extend(file_paths);
            }
            for route in file_routes {
                upsert_route(&mut routes, route);
            }
        }
        if let Some(ref info) = generated {
            let generated_pattern = vec![info.path.clone()];
            let (generated_routes, _) = load_routes_from_files(&generated_pattern)?;
            for route in generated_routes {
                upsert_route(&mut routes, route);
            }
        } else {
            // When not regenerating (no DB or generated_toml=false), load from any
            // previously-generated file on disk so restarts pick up DB-managed routes.
            let generated_path = config
                .generated_routes_dir()
                .join("routes.generated.toml");
            if generated_path.exists() {
                let pattern = vec![generated_path.to_string_lossy().to_string()];
                let (generated_routes, generated_files) = load_routes_from_files(&pattern)?;
                file_count += generated_routes.len();
                files.extend(generated_files);
                for route in generated_routes {
                    upsert_route(&mut routes, route);
                }
            }
        }

        routes.sort_by_key(|r| r.priority);
        let len = routes.len();
        *self.routes.write().unwrap() = routes;
        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds();
        info!(
            total = len,
            config_count, file_count, generated_entries, duration_ms, "routes reloaded"
        );
        Ok(ReloadMetrics {
            total: len,
            config_count,
            file_count,
            generated,
            files,
            patterns,
            started_at,
            finished_at,
            duration_ms,
        })
    }

    pub fn reload_acl_rules(
        &self,
        _generated_toml: bool,
        config_override: Option<Arc<ProxyConfig>>,
    ) -> Result<ReloadMetrics> {
        if let Some(config) = config_override {
            *self.config.write().unwrap() = config;
        }

        let config = self.config.read().unwrap().clone();

        let started_at = Utc::now();
        let mut rules: Vec<String> = Vec::new();
        let mut config_count = 0usize;
        let mut file_count = 0usize;
        let files_patterns = config.acl_files.clone();
        let mut files: Vec<String> = Vec::new();

        if let Some(cfg_rules) = config.acl_rules.clone() {
            config_count = cfg_rules.len();
            if config_count > 0 {
                info!(
                    count = config_count,
                    "loading acl rules from embedded config"
                );
            }
            rules.extend(cfg_rules);
        }

        if !config.acl_files.is_empty() {
            let (file_rules, file_paths) = load_acl_rules_from_files(&config.acl_files)?;
            file_count = file_rules.len();
            if !file_paths.is_empty() {
                files.extend(file_paths);
            }
            rules.extend(file_rules);
        }

        let generated_acl_path = config.generated_acl_dir().join("acl.generated.toml");
        if generated_acl_path.exists() {
            let generated_pattern = vec![generated_acl_path.to_string_lossy().to_string()];
            let (generated_rules, generated_files) = load_acl_rules_from_files(&generated_pattern)?;
            if !generated_files.is_empty() {
                files.extend(generated_files);
            }
            file_count += generated_rules.len();
            rules.extend(generated_rules);
        }

        if rules.is_empty() {
            rules.push("allow all".to_string());
            rules.push("deny all".to_string());
        }

        let len = rules.len();
        *self.acl_rules.write().unwrap() = rules;
        let finished_at = Utc::now();
        let duration_ms = (finished_at - started_at).num_milliseconds();
        info!(
            total = len,
            config_count, file_count, duration_ms, "acl rules reloaded"
        );
        Ok(ReloadMetrics {
            total: len,
            config_count,
            file_count,
            generated: None,
            files,
            patterns: files_patterns,
            started_at,
            finished_at,
            duration_ms,
        })
    }

    pub fn set_acl_rules(&self, mut rules: Vec<String>) {
        if rules.is_empty() {
            rules = vec!["allow all".to_string(), "deny all".to_string()];
        }

        let total = rules.len();
        *self.acl_rules.write().unwrap() = rules;
        info!(total = total, "acl rules snapshot updated at runtime");
    }

    async fn export_trunks_to_toml(
        &self,
        config: &ProxyConfig,
        default_dir: &Path,
    ) -> Result<Option<GeneratedFileMetrics>> {
        let Some(db) = self.db.as_ref() else {
            return Ok(None);
        };
        let Some(target_path) =
            resolve_generated_path(&config.trunks_files, default_dir, "trunks.generated.toml")
        else {
            return Ok(None);
        };

        let trunks = load_trunks_from_db(db).await?;
        let entries = trunks.len();
        let backup = backup_existing_file(&target_path)?;
        write_trunks_file(&target_path, &trunks)?;
        info!(path = %target_path.display(), entries, "generated trunks file from database");
        Ok(Some(GeneratedFileMetrics {
            entries,
            path: target_path.to_string_lossy().to_string(),
            backup: backup.map(|path| path.to_string_lossy().to_string()),
        }))
    }

    async fn export_routes_to_toml(
        &self,
        config: &ProxyConfig,
        default_dir: &Path,
    ) -> Result<Option<GeneratedFileMetrics>> {
        let Some(db) = self.db.as_ref() else {
            return Ok(None);
        };
        let Some(target_path) =
            resolve_generated_path(&config.routes_files, default_dir, "routes.generated.toml")
        else {
            return Ok(None);
        };

        let trunk_lookup = {
            let guard = self.trunks.read().unwrap();
            guard
                .iter()
                .filter_map(|(name, trunk)| trunk.id.map(|id| (id, name.clone())))
                .collect::<HashMap<i64, String>>()
        };

        let routes = load_routes_from_db(db, &trunk_lookup).await?;
        let entries = routes.len();
        let backup = backup_existing_file(&target_path)?;
        write_routes_file(&target_path, &routes)?;
        info!(path = %target_path.display(), entries, "generated routes file from database");
        Ok(Some(GeneratedFileMetrics {
            entries,
            path: target_path.to_string_lossy().to_string(),
            backup: backup.map(|path| path.to_string_lossy().to_string()),
        }))
    }
}

#[async_trait]
impl RouteResourceLookup for ProxyDataContext {
    async fn load_queue(&self, path: &str) -> Result<Option<RouteQueueConfig>> {
        self.resolve_queue_config(path)
    }
}

/// Trunk include-file shape. Accepts BOTH:
///
/// * **Legacy routing-format** — the historical shape used by the file-based
///   loader. SIP-only; routing-layer fields live at the top level under a
///   `[trunks.<name>]` map. Continues to work unchanged.
///
/// * **Kind-aware format (PR 2B)** — an array `[[trunk]]` of entries that
///   carry an explicit `kind` discriminator and a nested `[kind_config]`
///   table validated through `kind_schemas`. SIP entries are folded into
///   the routing `TrunkConfig` shape. Non-SIP entries (e.g. `kind = "webrtc"`)
///   validate successfully but are skipped from the routing map — the routing
///   layer only forwards SIP. WebRTC trunks are dispatched separately by the
///   bridge in Phase 7.
#[derive(Default, Deserialize, Serialize)]
struct TrunkIncludeFile {
    #[serde(default)]
    trunks: HashMap<String, TrunkConfig>,
    /// Kind-aware entries (new in PR 2B). Optional; absent in legacy files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    trunk: Vec<KindAwareTrunkEntry>,
}

/// Kind-aware trunk entry from a TOML include file. Routing-shared fields
/// (`name`, `direction`, `is_active`, optional caps, ACLs) live at the top
/// level; per-kind config lives under `[kind_config]`.
#[derive(Debug, Deserialize, Serialize)]
struct KindAwareTrunkEntry {
    pub name: String,
    #[serde(default = "default_kind_sip")]
    pub kind: String,
    #[serde(default = "default_true_loader")]
    pub is_active: bool,
    #[serde(default)]
    pub direction: Option<crate::models::sip_trunk::SipTrunkDirection>,
    #[serde(default)]
    pub max_cps: Option<u32>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub allowed_ips: Option<serde_json::Value>,
    /// Optional metadata blob — recording policy is folded from here.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// The kind-specific config. Required for non-SIP kinds. For SIP it
    /// may be omitted; in that case the loader falls back to gathering
    /// any legacy top-level SIP fields (see `legacy_sip`).
    #[serde(default)]
    pub kind_config: Option<serde_json::Value>,
    /// Legacy back-compat: when `kind` is absent (defaults to `"sip"`) and
    /// `kind_config` is absent, the deserializer accepts the historical
    /// top-level SIP fields here via `#[serde(flatten)]` into a catch-all.
    /// Documented as supported for one release cycle; new files should use
    /// the explicit `[kind_config]` block.
    #[serde(flatten)]
    pub legacy_sip: HashMap<String, serde_json::Value>,
}

fn default_kind_sip() -> String {
    "sip".to_string()
}

fn default_true_loader() -> bool {
    true
}

#[derive(Default, Deserialize, Serialize)]
struct RouteIncludeFile {
    #[serde(default)]
    routes: Vec<RouteRule>,
}

#[derive(Default, Deserialize, Serialize)]
struct AclIncludeFile {
    #[serde(default)]
    acl_rules: Vec<String>,
}

fn load_trunks_from_files(
    patterns: &[String],
) -> Result<(HashMap<String, TrunkConfig>, Vec<String>)> {
    let mut trunks: HashMap<String, TrunkConfig> = HashMap::new();
    let mut files: Vec<String> = Vec::new();
    for pattern in patterns {
        let entries = glob(pattern)
            .map_err(|e| anyhow!("invalid trunk include pattern '{}': {}", pattern, e))?;
        for entry in entries {
            let path =
                entry.map_err(|e| anyhow!("failed to read trunk include glob entry: {}", e))?;
            let path_display = path.display().to_string();
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read trunk include file {}", path_display))?;
            let data: TrunkIncludeFile = toml::from_str(&contents)
                .with_context(|| format!("failed to parse trunk include file {}", path_display))?;
            if !files.contains(&path_display) {
                files.push(path_display.clone());
            }
            if data.trunks.is_empty() && data.trunk.is_empty() {
                info!("trunk include file {} contained no trunks", path_display);
            }
            for (name, mut trunk) in data.trunks {
                info!("loaded trunk '{}' from {}", name, path_display);
                trunk.origin = ConfigOrigin::from_file(path_display.clone());
                trunks.insert(name, trunk);
            }
            for entry in data.trunk {
                match process_kind_aware_entry(entry, &path_display) {
                    Ok(Some((name, trunk))) => {
                        trunks.insert(name, trunk);
                    }
                    Ok(None) => {
                        // Non-SIP kind validated successfully but not added to
                        // the routing map (routing layer only forwards SIP).
                    }
                    Err(e) => {
                        warn!(
                            file = %path_display,
                            error = %e,
                            "skipping invalid trunk entry from include file"
                        );
                    }
                }
            }
        }
    }
    Ok((trunks, files))
}

/// Validate a kind-aware include-file trunk entry and, for SIP, project it
/// onto the routing-layer `TrunkConfig` shape. Returns:
///
/// * `Ok(Some((name, trunk)))` — SIP entry that should land in the routing map.
/// * `Ok(None)` — Non-SIP entry validated successfully but routing layer
///   doesn't consume it (e.g. WebRTC trunks, dispatched separately).
/// * `Err(_)` — Validation failed; caller logs and skips.
fn process_kind_aware_entry(
    entry: KindAwareTrunkEntry,
    path_display: &str,
) -> Result<Option<(String, TrunkConfig)>> {
    if entry.name.trim().is_empty() {
        return Err(anyhow!("trunk entry missing required `name`"));
    }

    // For SIP, fold any legacy top-level fields into `kind_config` so the
    // validator sees the complete blob — same tolerance as the REST CRUD.
    let kind_config_value = build_kind_config_value(&entry)?;

    // Single validation gate: every kind goes through `kind_schemas`.
    kind_schemas::validate(&entry.kind, &kind_config_value)
        .map_err(|e| anyhow!("trunk '{}' kind='{}': {}", entry.name, entry.kind, e))?;

    if entry.kind == "webrtc" {
        // Project WebRTC trunk file entry onto a minimal routing
        // `TrunkConfig`. Mirrors `convert_webrtc_trunk` for DB rows.
        let webrtc_cfg: crate::models::trunk::WebRtcTrunkConfig =
            serde_json::from_value(kind_config_value.clone()).map_err(|e| {
                anyhow!("trunk '{}': decode webrtc kind_config: {}", entry.name, e)
            })?;
        let trunk = TrunkConfig {
            dest: webrtc_cfg.endpoint_url.clone(),
            disabled: Some(!entry.is_active),
            max_calls: entry.max_concurrent,
            max_cps: entry.max_cps,
            direction: entry.direction.map(|d| d.into()),
            recording: entry
                .metadata
                .as_ref()
                .and_then(recording_policy_from_metadata),
            origin: ConfigOrigin::from_file(path_display.to_string()),
            kind: "webrtc".to_string(),
            kind_config: Some(kind_config_value),
            ..Default::default()
        };
        info!(
            file = %path_display,
            name = %entry.name,
            kind = %entry.kind,
            "loaded webrtc trunk from include file"
        );
        return Ok(Some((entry.name, trunk)));
    }
    if entry.kind == "external_media" {
        let trunk = TrunkConfig {
            dest: serde_json::from_value::<crate::models::trunk::ExternalMediaTrunkConfig>(
                kind_config_value.clone(),
            )
            .map_err(|e| {
                anyhow!(
                    "trunk '{}': decode external_media kind_config: {}",
                    entry.name,
                    e
                )
            })?
            .command,
            disabled: Some(!entry.is_active),
            max_calls: entry.max_concurrent,
            max_cps: entry.max_cps,
            direction: entry.direction.map(|d| d.into()),
            recording: entry
                .metadata
                .as_ref()
                .and_then(recording_policy_from_metadata),
            rewrite_hostport: true,
            origin: ConfigOrigin::from_file(path_display.to_string()),
            kind: "external_media".to_string(),
            kind_config: Some(kind_config_value),
            ..Default::default()
        };
        info!(
            file = %path_display,
            name = %entry.name,
            kind = %entry.kind,
            "loaded external_media trunk from include file"
        );
        return Ok(Some((entry.name, trunk)));
    }
    if entry.kind == "livekit" {
        let trunk = TrunkConfig {
            dest: serde_json::from_value::<crate::models::trunk::LiveKitTrunkConfig>(
                kind_config_value.clone(),
            )
            .map_err(|e| {
                anyhow!("trunk '{}': decode livekit kind_config: {}", entry.name, e)
            })?
            .server_url,
            disabled: Some(!entry.is_active),
            max_calls: entry.max_concurrent,
            max_cps: entry.max_cps,
            direction: entry.direction.map(|d| d.into()),
            recording: entry
                .metadata
                .as_ref()
                .and_then(recording_policy_from_metadata),
            rewrite_hostport: true,
            origin: ConfigOrigin::from_file(path_display.to_string()),
            kind: "livekit".to_string(),
            kind_config: Some(kind_config_value),
            ..Default::default()
        };
        info!(
            file = %path_display,
            name = %entry.name,
            kind = %entry.kind,
            "loaded livekit trunk from include file"
        );
        return Ok(Some((entry.name, trunk)));
    }
    if entry.kind != "sip" {
        info!(
            file = %path_display,
            name = %entry.name,
            kind = %entry.kind,
            "loaded non-SIP trunk from include file (validated; not added to routing map)"
        );
        return Ok(None);
    }

    // SIP path: project onto the routing `TrunkConfig` shape — same logic
    // as `convert_trunk` for DB rows.
    let sip_cfg: SipTrunkConfig = serde_json::from_value(kind_config_value)
        .map_err(|e| anyhow!("trunk '{}': decode sip kind_config: {}", entry.name, e))?;

    let primary = sip_cfg.sip_server.clone().or(sip_cfg.outbound_proxy.clone());
    let Some(dest) = primary else {
        return Err(anyhow!(
            "sip trunk '{}' missing both sip_server and outbound_proxy",
            entry.name
        ));
    };
    let backup_dest = sip_cfg
        .outbound_proxy
        .clone()
        .filter(|outbound| *outbound != dest);
    let transport = Some(sip_cfg.sip_transport.as_str().to_string());

    let mut inbound_hosts = extract_string_array(entry.allowed_ips.clone());
    if let Some(host) = extract_host_from_uri(&dest)
        && host.parse::<IpAddr>().is_ok()
    {
        push_unique(&mut inbound_hosts, host);
    }
    if let Some(backup) = &backup_dest
        && let Some(host) = extract_host_from_uri(backup)
        && host.parse::<IpAddr>().is_ok()
    {
        push_unique(&mut inbound_hosts, host);
    }

    let recording = entry
        .metadata
        .as_ref()
        .and_then(recording_policy_from_metadata);

    let trunk = TrunkConfig {
        dest,
        backup_dest,
        username: sip_cfg.auth_username,
        password: sip_cfg.auth_password,
        codec: Vec::new(),
        disabled: Some(!entry.is_active),
        max_calls: entry.max_concurrent,
        max_cps: entry.max_cps,
        weight: None,
        transport,
        id: None,
        direction: entry.direction.map(|d| d.into()),
        inbound_hosts,
        recording,
        incoming_from_user_prefix: sip_cfg.incoming_from_user_prefix,
        incoming_to_user_prefix: sip_cfg.incoming_to_user_prefix,
        country: None,
        policy: None,
        register_enabled: if sip_cfg.register_enabled {
            Some(true)
        } else {
            None
        },
        register_expires: sip_cfg.register_expires.map(|v| v as u32),
        register_extra_headers: sip_cfg
            .register_extra_headers
            .map(|pairs| pairs.into_iter().collect()),
        rewrite_hostport: sip_cfg.rewrite_hostport,
        origin: ConfigOrigin::from_file(path_display.to_string()),
        kind: "sip".to_string(),
        kind_config: None,
        ..Default::default()
    };

    info!(
        file = %path_display,
        name = %entry.name,
        kind = %entry.kind,
        "loaded kind-aware trunk from include file"
    );
    Ok(Some((entry.name, trunk)))
}

/// Compose the `kind_config` JSON value the validator will consume:
///
/// * If `kind_config` is present, use it verbatim (canonical nested format).
/// * Else if `kind == "sip"`, gather any legacy top-level SIP fields into a
///   synthetic object so the validator can apply the same checks.
/// * Else, error — non-SIP kinds must use the explicit `[kind_config]` form.
fn build_kind_config_value(entry: &KindAwareTrunkEntry) -> Result<serde_json::Value> {
    if let Some(v) = &entry.kind_config {
        return Ok(v.clone());
    }
    if entry.kind == "sip" {
        // Whitelist the known legacy SIP field names so we don't sweep in
        // unrelated top-level keys.
        const SIP_LEGACY_KEYS: &[&str] = &[
            "sip_server",
            "sip_transport",
            "outbound_proxy",
            "auth_username",
            "auth_password",
            "register_enabled",
            "register_expires",
            "register_extra_headers",
            "rewrite_hostport",
            "did_numbers",
            "incoming_from_user_prefix",
            "incoming_to_user_prefix",
            "default_route_label",
            "billing_snapshot",
            "analytics",
            "carrier",
        ];
        let mut map = serde_json::Map::new();
        for k in SIP_LEGACY_KEYS {
            if let Some(v) = entry.legacy_sip.get(*k) {
                map.insert((*k).to_string(), v.clone());
            }
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(anyhow!(
        "trunk '{}' kind='{}': missing required `kind_config`",
        entry.name,
        entry.kind
    ))
}

fn load_routes_from_files(patterns: &[String]) -> Result<(Vec<RouteRule>, Vec<String>)> {
    let mut routes: Vec<RouteRule> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for pattern in patterns {
        let entries = glob(pattern)
            .map_err(|e| anyhow!("invalid route include pattern '{}': {}", pattern, e))?;
        for entry in entries {
            let path =
                entry.map_err(|e| anyhow!("failed to read route include glob entry: {}", e))?;
            let path_display = path.display().to_string();
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read route include file {}", path_display))?;
            let data: RouteIncludeFile = toml::from_str(&contents)
                .with_context(|| format!("failed to parse route include file {}", path_display))?;
            if !files.contains(&path_display) {
                files.push(path_display.clone());
            }
            if data.routes.is_empty() {
                info!("route include file {} contained no routes", path_display);
            }
            for mut route in data.routes {
                info!("loaded route '{}' from {}", route.name, path_display);
                route.origin = ConfigOrigin::from_file(path_display.clone());
                upsert_route(&mut routes, route);
            }
        }
    }
    Ok((routes, files))
}

fn load_acl_rules_from_files(patterns: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    let mut rules: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for pattern in patterns {
        let entries = glob(pattern)
            .map_err(|e| anyhow!("invalid acl include pattern '{}': {}", pattern, e))?;
        for entry in entries {
            let path =
                entry.map_err(|e| anyhow!("failed to read acl include glob entry: {}", e))?;
            let path_display = path.display().to_string();
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read acl include file {}", path_display))?;
            let data: AclIncludeFile = toml::from_str(&contents)
                .with_context(|| format!("failed to parse acl include file {}", path_display))?;
            if !files.contains(&path_display) {
                files.push(path_display.clone());
            }
            if data.acl_rules.is_empty() {
                info!("acl include file {} contained no rules", path_display);
            }
            for rule in data.acl_rules {
                info!("loaded acl rule '{}' from {}", rule, path_display);
                rules.push(rule);
            }
        }
    }
    Ok((rules, files))
}

fn upsert_route(routes: &mut Vec<RouteRule>, route: RouteRule) {
    info!("upserted route '{}'", route.name);
    if let Some(idx) = routes
        .iter()
        .position(|existing| existing.name == route.name)
    {
        routes[idx] = route;
    } else {
        routes.push(route);
    }
}

fn contains_glob_chars(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
}

fn resolve_generated_path(
    patterns: &[String],
    default_dir: &Path,
    default_name: &str,
) -> Option<PathBuf> {
    for pattern in patterns {
        if pattern.trim().is_empty() {
            continue;
        }
        let path = Path::new(pattern);
        if contains_glob_chars(pattern) {
            if let Some(parent) = path.parent() {
                if parent.as_os_str().is_empty() {
                    return Some(default_dir.join(default_name));
                }
                return Some(parent.to_path_buf().join(default_name));
            }
            return Some(default_dir.join(default_name));
        } else {
            return Some(path.to_path_buf());
        }
    }
    Some(default_dir.join(default_name))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    Ok(())
}

fn backup_existing_file(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    let backup_name = format!("{}.{}.bak", file_name, timestamp);
    let backup_path = path.with_file_name(backup_name);
    fs::rename(path, &backup_path).with_context(|| {
        format!(
            "failed to backup {} to {}",
            path.display(),
            backup_path.display()
        )
    })?;
    Ok(Some(backup_path))
}

fn write_trunks_file(path: &Path, trunks: &HashMap<String, TrunkConfig>) -> Result<()> {
    ensure_parent_dir(path)?;
    let data = TrunkIncludeFile {
        trunks: trunks
            .iter()
            .map(|(name, trunk)| (name.clone(), trunk.clone()))
            .collect(),
        trunk: Vec::new(),
    };
    // Round-trip via serde_json then post-process for TOML compatibility:
    //   1. Strip null values (TOML has no null).
    //   2. Within every Object, emit scalar / array-of-scalar keys before any
    //      nested table / array-of-tables keys. TOML disallows "value after
    //      table" — without this, a webrtc trunk's `kind_config` with a
    //      nested `protocol` table followed by a sibling `signaling` string
    //      fails to serialize.
    let mut json_value = serde_json::to_value(&data)
        .with_context(|| format!("failed to convert trunks to json for {}", path.display()))?;
    normalize_for_toml(&mut json_value);
    let toml_value: toml::Value = serde_json::from_value(json_value).with_context(|| {
        format!(
            "failed to convert normalized trunks json to toml value for {}",
            path.display()
        )
    })?;
    let toml = toml::to_string_pretty(&toml_value)
        .with_context(|| format!("failed to serialize trunks toml for {}", path.display()))?;
    fs::write(path, toml)
        .with_context(|| format!("failed to write trunks file {}", path.display()))?;
    Ok(())
}

/// Normalize a JSON value tree for TOML serialization:
///   - drop `Value::Null` entries (TOML has no null);
///   - within every `Object`, partition keys so that scalar / array-of-scalar
///     keys come strictly before any nested `Object` / array-of-`Object` keys
///     (TOML disallows "value after table").
fn normalize_for_toml(value: &mut serde_json::Value) {
    fn is_table_like(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Object(_) => true,
            serde_json::Value::Array(arr) => arr.iter().any(is_table_like),
            _ => false,
        }
    }
    match value {
        serde_json::Value::Object(obj) => {
            // Strip null-valued entries.
            obj.retain(|_, v| !v.is_null());
            // Recurse into remaining values first so nested objects/arrays
            // are already normalized before we partition this level.
            for (_, v) in obj.iter_mut() {
                normalize_for_toml(v);
            }
            // Re-emit with stable key order: scalars first, tables last.
            let mut entries: Vec<(String, serde_json::Value)> =
                std::mem::take(obj).into_iter().collect();
            entries.sort_by(|a, b| {
                let a_table = is_table_like(&a.1);
                let b_table = is_table_like(&b.1);
                a_table.cmp(&b_table).then_with(|| a.0.cmp(&b.0))
            });
            for (k, v) in entries {
                obj.insert(k, v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                normalize_for_toml(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod write_trunks_toml_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_strips_null_and_reorders_tables_last() {
        let mut v = json!({
            "audio_codec": "opus",
            "auth_header": null,
            "protocol": {"request_body_template": "{}", "response_answer_path": "$.sdp"},
            "signaling": "http_json",
            "ice_servers": [{"urls": "stun:..."}]
        });
        normalize_for_toml(&mut v);
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("auth_header"), "null stripped");
        let order: Vec<&String> = obj.keys().collect();
        // All non-table-like keys must precede all table-like keys.
        let first_table_idx = order
            .iter()
            .position(|k| matches!(k.as_str(), "ice_servers" | "protocol"))
            .unwrap();
        for (i, k) in order.iter().enumerate() {
            if i < first_table_idx {
                assert!(
                    !matches!(k.as_str(), "ice_servers" | "protocol"),
                    "scalar key '{}' should come before any table",
                    k
                );
            }
        }
    }

    #[test]
    fn webrtc_kind_config_serializes_to_toml() {
        // Mirrors the shape `pipecat_bot` ends up with after promotion.
        let mut trunk = TrunkConfig::default();
        trunk.dest = "http://127.0.0.1:7860/api/offer".into();
        trunk.transport = Some("udp".into());
        trunk.id = Some(14);
        trunk.direction = Some(crate::proxy::routing::TrunkDirection::Outbound);
        trunk.disabled = Some(false);
        trunk.kind = "webrtc".into();
        trunk.kind_config = Some(json!({
            "audio_codec": "opus",
            "auth_header": null,
            "endpoint_url": "http://127.0.0.1:7860/api/offer",
            "health_check_url": "http://127.0.0.1:7860/healthz",
            "ice_servers": [{"urls": "stun:stun.l.google.com:19302"}],
            "protocol": {
                "request_body_template": "{\"sdp\":\"{offer_sdp}\",\"type\":\"offer\"}",
                "response_answer_path": "$.sdp",
                "response_session_path": "$.pc_id"
            },
            "signaling": "http_json"
        }));
        let mut map = HashMap::new();
        map.insert("pipecat_bot".to_string(), trunk);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        write_trunks_file(&path, &map).expect("toml serialize should succeed");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("pipecat_bot"),
            "round-trip writes back the trunk: {written}"
        );
        // Re-parse to confirm the file is valid TOML and our loader still
        // accepts it.
        let parsed: TrunkIncludeFile =
            toml::from_str(&written).expect("re-parse round-tripped TOML");
        assert!(parsed.trunks.contains_key("pipecat_bot"));
    }
}

fn write_routes_file(path: &Path, routes: &[RouteRule]) -> Result<()> {
    ensure_parent_dir(path)?;
    let data = RouteIncludeFile {
        routes: routes.to_vec(),
    };
    let toml = toml::to_string_pretty(&data)
        .with_context(|| format!("failed to serialize routes toml for {}", path.display()))?;
    fs::write(path, toml)
        .with_context(|| format!("failed to write routes file {}", path.display()))?;
    Ok(())
}

async fn load_trunks_from_db(db: &DatabaseConnection) -> Result<HashMap<String, TrunkConfig>> {
    // Both SIP and WebRTC trunks land in the routing-layer cache so the
    // matcher can branch by kind in the `Forward` arm (see Phase 7). Other
    // (unknown) kinds are skipped with a warning by `convert_trunk`.
    let models = sip_trunk::Entity::find()
        .filter(sip_trunk::Column::IsActive.eq(true))
        .order_by_asc(sip_trunk::Column::Name)
        .all(db)
        .await?;

    let mut trunks = HashMap::new();
    for model in models {
        match convert_trunk(model) {
            Ok(Some((name, trunk))) => {
                trunks.insert(name, trunk);
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, "skipping trunk row with invalid kind_config");
            }
        }
    }
    Ok(trunks)
}

/// Extract SBC-specific config from `metadata.sbc` JSON blob.
pub fn sbc_config_from_metadata(meta: &serde_json::Value) -> TrunkConfig {
    let sbc = meta.get("sbc");
    let from_str = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str().map(String::from));
    let from_u64 = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_u64().map(|n| n as u32));
    let from_bool = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_bool());
    let from_strs = |v: Option<&serde_json::Value>| -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };

    let codecs = from_strs(sbc.and_then(|s| s.get("codecs")));
    let recording = from_bool(sbc.and_then(|s| s.get("recording_enabled"))).map(|enabled| {
        let mut r = crate::config::RecordingPolicy::default();
        r.enabled = enabled;
        r
    });

    TrunkConfig {
        call_id_mode: from_str(sbc.and_then(|s| s.get("call_id_mode"))).and_then(|s| match s.as_str() {
            "transparent" => Some(CallIdMode::Transparent),
            "rewrite" => Some(CallIdMode::Rewrite),
            _ => None,
        }),
        health_check_enabled: from_bool(sbc.and_then(|s| s.get("health_check_enabled"))),
        health_check_interval_secs: sbc.and_then(|s| s.get("health_check_interval")).and_then(|v| v.as_u64()),
        health_check_probe_count: from_u64(sbc.and_then(|s| s.get("health_check_probe_count"))),
        health_check_fallback_trunk: from_str(sbc.and_then(|s| s.get("health_check_fallback_trunk"))),
        cac_policy: from_str(sbc.and_then(|s| s.get("cac_policy"))).and_then(|s| match s.as_str() {
            "lossy" => Some(CacPolicy::Lossy),
            "reject" => Some(CacPolicy::Reject),
            "overflow" => Some(CacPolicy::Overflow),
            _ => None,
        }),
        overflow_threshold: from_u64(sbc.and_then(|s| s.get("overflow_threshold"))),
        media_mode: from_str(sbc.and_then(|s| s.get("media_mode"))).and_then(|s| match s.as_str() {
            "none" => Some(MediaMode::None),
            "bypass" => Some(MediaMode::Bypass),
            "auto" => Some(MediaMode::Auto),
            "force_transcode" => Some(MediaMode::ForceTranscode),
            _ => None,
        }),
        video_policy: from_str(sbc.and_then(|s| s.get("video_policy"))).and_then(|s| match s.as_str() {
            "pass_through" => Some(VideoPolicy::PassThrough),
            "strip" => Some(VideoPolicy::Strip),
            "transcode" => Some(VideoPolicy::Transcode),
            _ => None,
        }),
        header_rules: sbc.and_then(|s| s.get("header_rules")).and_then(|v| serde_json::from_value(v.clone()).ok()),
        codec: codecs,
        recording: if recording.is_some() { recording } else { None },
        ..Default::default()
    }
}

fn convert_trunk(model: sip_trunk::Model) -> Result<Option<(String, TrunkConfig)>> {
    // Branch on kind. SIP trunks project onto the full SIP-shaped
    // `TrunkConfig`. WebRTC trunks get a minimal projection — only the
    // routing layer's required fields (`dest`, `direction`, capacity caps)
    // are populated; the matcher detects them via `kind="webrtc"` and routes
    // them into the bridge dispatcher instead of the SIP forward path.
    match model.kind.as_str() {
        "sip" => convert_sip_trunk(model),
        "webrtc" => convert_webrtc_trunk(model),
        "livekit" => convert_livekit_trunk(model),
        "external_media" => convert_external_media_trunk(model),
        other => {
            warn!(
                trunk = %model.name,
                kind = %other,
                "skipping trunk with unsupported kind"
            );
            Ok(None)
        }
    }
}

fn convert_sip_trunk(model: sip_trunk::Model) -> Result<Option<(String, TrunkConfig)>> {
    // Parse the kind-specific config once; downstream reads off the typed
    // struct rather than re-parsing JSON per field.
    let sip_cfg = model.sip()?;

    let primary = sip_cfg.sip_server.clone().or(sip_cfg.outbound_proxy.clone());
    let Some(dest) = primary else {
        return Ok(None);
    };

    let backup_dest = sip_cfg
        .outbound_proxy
        .clone()
        .filter(|outbound| *outbound != dest);

    let transport = Some(sip_cfg.sip_transport.as_str().to_string());

    let mut inbound_hosts = extract_string_array(model.allowed_ips.clone());
    if let Some(host) = extract_host_from_uri(&dest)
        && host.parse::<IpAddr>().is_ok()
    {
        push_unique(&mut inbound_hosts, host);
    }
    if let Some(backup) = &backup_dest
        && let Some(host) = extract_host_from_uri(backup)
        && host.parse::<IpAddr>().is_ok()
    {
        push_unique(&mut inbound_hosts, host);
    }

    let recording = model
        .metadata
        .as_ref()
        .and_then(recording_policy_from_metadata);
    let codec = sip_trunk_codecs(model.metadata.as_ref());

    // Base trunk config
    let mut trunk = TrunkConfig {
        dest,
        backup_dest,
        username: sip_cfg.auth_username,
        password: sip_cfg.auth_password,
        codec,
        disabled: Some(!model.is_active),
        max_calls: model.max_concurrent.map(|v| v as u32),
        max_cps: model.max_cps.map(|v| v as u32),
        weight: None,
        transport,
        id: Some(model.id),
        direction: Some(model.direction.into()),
        inbound_hosts,
        recording,
        incoming_from_user_prefix: sip_cfg.incoming_from_user_prefix,
        incoming_to_user_prefix: sip_cfg.incoming_to_user_prefix,
        country: None,
        policy: None,
        register_enabled: if sip_cfg.register_enabled {
            Some(true)
        } else {
            None
        },
        register_expires: sip_cfg.register_expires.map(|v| v as u32),
        register_extra_headers: sip_cfg
            .register_extra_headers
            .map(|pairs| pairs.into_iter().collect()),
        rewrite_hostport: sip_cfg.rewrite_hostport,
        did_numbers: sip_cfg.did_numbers
            .and_then(|v| serde_json::from_value::<Vec<String>>(v).ok())
            .unwrap_or_default(),
        origin: ConfigOrigin::embedded(),
        kind: "sip".to_string(),
        kind_config: None,
        ..Default::default()
    };

    // Merge SBC metadata overrides (only if the corresponding metadata field exists)
    if let Some(ref meta) = model.metadata {
        if meta.get("sbc").is_some() {
            let sbc = sbc_config_from_metadata(meta);
            if sbc.call_id_mode.is_some() { trunk.call_id_mode = sbc.call_id_mode; }
            if sbc.health_check_enabled.is_some() { trunk.health_check_enabled = sbc.health_check_enabled; }
            if sbc.health_check_interval_secs.is_some() { trunk.health_check_interval_secs = sbc.health_check_interval_secs; }
            if sbc.health_check_probe_count.is_some() { trunk.health_check_probe_count = sbc.health_check_probe_count; }
            if sbc.health_check_fallback_trunk.is_some() { trunk.health_check_fallback_trunk = sbc.health_check_fallback_trunk; }
            if sbc.cac_policy.is_some() { trunk.cac_policy = sbc.cac_policy; }
            if sbc.overflow_threshold.is_some() { trunk.overflow_threshold = sbc.overflow_threshold; }
            if sbc.media_mode.is_some() { trunk.media_mode = sbc.media_mode; }
            if sbc.video_policy.is_some() { trunk.video_policy = sbc.video_policy; }
            if sbc.header_rules.is_some() { trunk.header_rules = sbc.header_rules; }
            if !sbc.codec.is_empty() { trunk.codec = sbc.codec; }
            if sbc.recording.is_some() { trunk.recording = sbc.recording; }
        }
    }

    Ok(Some((model.name, trunk)))
}

/// Project a `kind="webrtc"` trunk row onto the routing-layer `TrunkConfig`.
///
/// The routing matcher only needs enough to identify the trunk, run shared
/// trunk-policy/capacity checks, and detect `kind="webrtc"` so the Forward
/// arm short-circuits into [`crate::proxy::bridge::dispatch_webrtc`]. The
/// full webrtc-specific config (signaling adapter, endpoint URL, protocol,
/// ICE servers) is preserved verbatim in `kind_config` so the dispatcher
/// can read it without a second DB roundtrip.
fn convert_webrtc_trunk(model: sip_trunk::Model) -> Result<Option<(String, TrunkConfig)>> {
    let webrtc_cfg = model.webrtc()?;
    // Use the signaling endpoint URL as the trunk's `dest` so logging and
    // diagnostics have something human-meaningful, and so anything
    // downstream that key-by-dest doesn't crash on an empty string.
    let dest = webrtc_cfg.endpoint_url.clone();

    let trunk = TrunkConfig {
        dest,
        backup_dest: None,
        username: None,
        password: None,
        codec: Vec::new(),
        disabled: Some(!model.is_active),
        max_calls: model.max_concurrent.map(|v| v as u32),
        max_cps: model.max_cps.map(|v| v as u32),
        weight: None,
        transport: None,
        id: Some(model.id),
        direction: Some(model.direction.into()),
        inbound_hosts: Vec::new(),
        recording: model
            .metadata
            .as_ref()
            .and_then(recording_policy_from_metadata),
        incoming_from_user_prefix: None,
        incoming_to_user_prefix: None,
        country: None,
        policy: None,
        register_enabled: None,
        register_expires: None,
        register_extra_headers: None,
        rewrite_hostport: true,
        origin: ConfigOrigin::embedded(),
        kind: "webrtc".to_string(),
        kind_config: Some(model.kind_config.clone()),
        ..Default::default()
    };

    Ok(Some((model.name, trunk)))
}

fn convert_livekit_trunk(model: sip_trunk::Model) -> Result<Option<(String, TrunkConfig)>> {
    let livekit_cfg = model.livekit()?;
    let dest = livekit_cfg.server_url.clone();

    let trunk = TrunkConfig {
        dest,
        disabled: Some(!model.is_active),
        max_calls: model.max_concurrent.map(|v| v as u32),
        max_cps: model.max_cps.map(|v| v as u32),
        id: Some(model.id),
        direction: Some(model.direction.into()),
        recording: model
            .metadata
            .as_ref()
            .and_then(recording_policy_from_metadata),
        rewrite_hostport: true,
        origin: ConfigOrigin::embedded(),
        kind: "livekit".to_string(),
        kind_config: Some(model.kind_config.clone()),
        ..Default::default()
    };

    Ok(Some((model.name, trunk)))
}

/// Project a `kind="external_media"` trunk row onto the routing-layer
/// `TrunkConfig`. `dest` is the sidecar spawn command (no SIP server — the
/// matcher short-circuits this kind into the external_media dispatcher);
/// the full config stays in `kind_config` for the dispatcher.
fn convert_external_media_trunk(
    model: sip_trunk::Model,
) -> Result<Option<(String, TrunkConfig)>> {
    let cfg = model.external_media()?;
    let trunk = TrunkConfig {
        dest: cfg.command.clone(),
        disabled: Some(!model.is_active),
        max_calls: model.max_concurrent.map(|v| v as u32),
        max_cps: model.max_cps.map(|v| v as u32),
        id: Some(model.id),
        direction: Some(model.direction.into()),
        recording: model
            .metadata
            .as_ref()
            .and_then(recording_policy_from_metadata),
        rewrite_hostport: true,
        origin: ConfigOrigin::embedded(),
        kind: "external_media".to_string(),
        kind_config: Some(model.kind_config.clone()),
        ..Default::default()
    };

    Ok(Some((model.name, trunk)))
}

pub(crate) async fn load_routes_from_db(
    db: &DatabaseConnection,
    trunk_lookup: &HashMap<i64, String>,
) -> Result<Vec<RouteRule>> {
    let models = routing::Entity::find()
        .filter(routing::Column::IsActive.eq(true))
        .order_by_asc(routing::Column::Priority)
        .all(db)
        .await?;

    let mut routes = Vec::new();
    for model in models {
        if let Some(route) = convert_route(model, trunk_lookup).context("convert route")? {
            routes.push(route);
        }
    }
    Ok(routes)
}

fn recording_policy_from_metadata(value: &serde_json::Value) -> Option<RecordingPolicy> {
    value
        .get("recording")
        .and_then(|entry| serde_json::from_value::<RecordingPolicy>(entry.clone()).ok())
}

/// Default HD codec-upgrade offer for SIP trunks, best-quality first with
/// G729 as last resort. The matcher pins this as the egress offer and flips
/// to the Quality strategy (see `call.rs`), so a low-quality caller is
/// transcoded/resampled UP to the best codec the callee accepts.
const DEFAULT_HD_CODECS: [&str; 5] = ["opus", "g722", "pcmu", "pcma", "g729"];

/// Resolve a SIP trunk's egress codec list. HD upgrade is **ON by default**:
/// absent config yields [`DEFAULT_HD_CODECS`]. An operator opts a trunk OUT
/// with `metadata.media.hd_disabled = true`, yielding an empty list so codec
/// selection falls back to the compiled defaults in `negotiate.rs` (no forced
/// upgrade). An explicit `metadata.media.codecs` list overrides the default.
fn sip_trunk_codecs(metadata: Option<&serde_json::Value>) -> Vec<String> {
    let media = metadata.and_then(|m| m.get("media"));
    if media
        .and_then(|m| m.get("hd_disabled"))
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return Vec::new();
    }
    media
        .and_then(|m| m.get("codecs"))
        .and_then(|c| serde_json::from_value::<Vec<String>>(c.clone()).ok())
        .filter(|list| !list.is_empty())
        .unwrap_or_else(|| DEFAULT_HD_CODECS.iter().map(|s| s.to_string()).collect())
}

#[derive(Debug, Default, Deserialize)]
struct RouteMetadataDocument {
    #[serde(default)]
    action: Option<RouteMetadataAction>,
}

#[derive(Debug, Default, Deserialize)]
struct RouteMetadataAction {
    #[serde(default)]
    target_type: Option<String>,
    #[serde(default)]
    queue_file: Option<String>,
    #[serde(default)]
    voicemail_extension: Option<String>,
    #[serde(default)]
    ivr_file: Option<String>,
}

fn convert_route(
    model: routing::Model,
    trunk_lookup: &HashMap<i64, String>,
) -> Result<Option<RouteRule>> {
    let mut match_conditions = MatchConditions::default();
    if let Some(pattern) = model.source_pattern.clone()
        && !pattern.is_empty()
    {
        match_conditions.from_user = Some(pattern);
    }
    if let Some(pattern) = model.destination_pattern.clone()
        && !pattern.is_empty()
    {
        match_conditions.to_user = Some(pattern);
    }

    if let Some(filters) = model.header_filters.clone()
        && let Ok(map) = serde_json::from_value::<HashMap<String, String>>(filters)
    {
        apply_match_filters(&mut match_conditions, map);
    }
    finalize_match_conditions(&mut match_conditions);

    let rewrite_rules = model
        .rewrite_rules
        .clone()
        .and_then(|value| serde_json::from_value::<RewriteRules>(value).ok())
        .map(|mut rules| {
            normalize_rewrite_rules(&mut rules);
            rules
        });

    // Accept multiple shapes for `target_trunks`:
    //   1. [{"name": "pipecat_bot"}]              — legacy/internal shape
    //   2. [{"trunk_name": "pipecat_bot"}]        — REST-API wire shape
    //   3. ["pipecat_bot"]                        — bare string list
    // All collapse to a Vec<String> of trunk names. Empty / unparseable
    // entries are silently skipped (preserves prior tolerant behavior).
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RouteTrunkDocument {
        Object {
            #[serde(default, alias = "trunk_name")]
            name: Option<String>,
        },
        Bare(String),
    }

    let target_trunks: Vec<String> = model
        .target_trunks
        .clone()
        .and_then(|value| serde_json::from_value::<Vec<RouteTrunkDocument>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|trunk| match trunk {
            RouteTrunkDocument::Object { name } => name.filter(|s| !s.is_empty()),
            RouteTrunkDocument::Bare(s) => Some(s).filter(|s| !s.is_empty()),
        })
        .collect::<Vec<_>>();

    let dest = if target_trunks.is_empty() {
        None
    } else if target_trunks.len() == 1 {
        Some(DestConfig::Single(target_trunks[0].clone()))
    } else {
        Some(DestConfig::Multiple(target_trunks))
    };

    let mut action = RouteAction::default();
    if let Some(dest) = dest {
        action.dest = Some(dest);
    }
    action.select = model.selection_strategy.as_str().to_string();
    action.hash_key = model.hash_key.clone();

    if let Some(metadata) = model.metadata.clone()
        && let Ok(doc) = serde_json::from_value::<RouteMetadataDocument>(metadata)
        && let Some(meta_action) = doc.action
    {
        apply_route_metadata(&mut action, meta_action);
    }

    let direction = match model.direction {
        routing::RoutingDirection::Inbound => RouteDirection::Inbound,
        routing::RoutingDirection::Outbound => RouteDirection::Outbound,
    };

    let mut source_trunks = Vec::new();
    let mut source_trunk_ids = Vec::new();
    if let Some(id) = model.source_trunk_id {
        source_trunk_ids.push(id);
        if let Some(name) = trunk_lookup.get(&id) {
            source_trunks.push(name.clone());
        }
    }

    let route = RouteRule {
        name: model.name,
        db_id: Some(model.id),
        description: model.description,
        priority: model.priority,
        direction,
        source_trunks,
        source_trunk_ids,
        match_conditions,
        rewrite: rewrite_rules,
        action,
        disabled: Some(!model.is_active),
        policy: None,
        origin: ConfigOrigin::embedded(),
        codecs: Vec::new(),
        disable_ice_servers: None,
    };
    Ok(Some(route))
}

fn apply_route_metadata(action: &mut RouteAction, meta: RouteMetadataAction) {
    let target_type = meta
        .target_type
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "sip_trunk".to_string());

    match target_type.as_str() {
        "queue" => {
            if let Some(queue_path) = sanitize_metadata_string(meta.queue_file) {
                action.queue = Some(queue_path);
            }
        }
        "voicemail" => {
            let ext = sanitize_metadata_string(meta.voicemail_extension).unwrap_or_default();
            action.app = Some("voicemail".to_string());
            action.app_params = Some(serde_json::json!({ "extension": ext }));
        }
        "ivr" => {
            if let Some(file) = sanitize_metadata_string(meta.ivr_file) {
                action.app = Some("ivr".to_string());
                action.app_params = Some(serde_json::json!({ "file": file }));
            }
        }
        _ => {}
    }
}

fn set_field(target: &mut Option<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    match target {
        Some(existing) if existing == trimmed => {}
        _ => *target = Some(trimmed.to_string()),
    }
}

fn sanitize_metadata_string(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|trimmed| !trimmed.is_empty())
}

fn canonical_condition_key(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(['_', '-'], ".")
}

fn handle_match_key(match_conditions: &mut MatchConditions, key: &str, value: &str) -> bool {
    let trimmed_key = key.trim();
    if trimmed_key.is_empty() {
        return true;
    }
    let canonical = canonical_condition_key(trimmed_key);
    match canonical.as_str() {
        "from.user" | "caller" | "from" => {
            set_field(&mut match_conditions.from_user, value);
            true
        }
        "from.host" => {
            set_field(&mut match_conditions.from_host, value);
            true
        }
        "to.user" | "callee" | "to" => {
            set_field(&mut match_conditions.to_user, value);
            true
        }
        "to.host" => {
            set_field(&mut match_conditions.to_host, value);
            true
        }
        "to.port" => {
            set_field(&mut match_conditions.to_port, value);
            true
        }
        "request.uri.user" => {
            set_field(&mut match_conditions.request_uri_user, value);
            true
        }
        "request.uri.host" => {
            set_field(&mut match_conditions.request_uri_host, value);
            true
        }
        "request.uri.port" => {
            set_field(&mut match_conditions.request_uri_port, value);
            true
        }
        _ => false,
    }
}

fn apply_match_filters(match_conditions: &mut MatchConditions, map: HashMap<String, String>) {
    let mut headers = HashMap::new();
    for (key, raw_value) in map {
        let value = raw_value.trim();
        if value.is_empty() {
            continue;
        }
        if handle_match_key(match_conditions, &key, value) {
            continue;
        }
        headers.insert(key.trim().to_string(), value.to_string());
    }
    match_conditions.headers = headers;
}

fn finalize_match_conditions(match_conditions: &mut MatchConditions) {
    if let Some(value) = match_conditions.from.take() {
        set_field(&mut match_conditions.from_user, value.as_str());
    }
    if let Some(value) = match_conditions.caller.take() {
        set_field(&mut match_conditions.from_user, value.as_str());
    }
    if let Some(value) = match_conditions.to.take() {
        set_field(&mut match_conditions.to_user, value.as_str());
    }
    if let Some(value) = match_conditions.callee.take() {
        set_field(&mut match_conditions.to_user, value.as_str());
    }

    let entries = std::mem::take(&mut match_conditions.headers);
    for (key, raw_value) in entries {
        let trimmed_key = key.trim();
        if trimmed_key.is_empty() {
            continue;
        }
        let value = raw_value.trim();
        if value.is_empty() {
            continue;
        }
        if handle_match_key(match_conditions, trimmed_key, value) {
            continue;
        }
        match_conditions
            .headers
            .insert(trimmed_key.to_string(), value.to_string());
    }
}

fn handle_rewrite_key(rules: &mut RewriteRules, key: &str, value: &str) -> bool {
    let trimmed_key = key.trim();
    if trimmed_key.is_empty() {
        return true;
    }
    let canonical = canonical_condition_key(trimmed_key);
    match canonical.as_str() {
        "from.user" => {
            set_field(&mut rules.from_user, value);
            true
        }
        "from.host" => {
            set_field(&mut rules.from_host, value);
            true
        }
        "to.user" => {
            set_field(&mut rules.to_user, value);
            true
        }
        "to.host" => {
            set_field(&mut rules.to_host, value);
            true
        }
        "to.port" => {
            set_field(&mut rules.to_port, value);
            true
        }
        "request.uri.user" => {
            set_field(&mut rules.request_uri_user, value);
            true
        }
        "request.uri.host" => {
            set_field(&mut rules.request_uri_host, value);
            true
        }
        "request.uri.port" => {
            set_field(&mut rules.request_uri_port, value);
            true
        }
        _ => false,
    }
}

fn normalize_rewrite_rules(rules: &mut RewriteRules) {
    let mut headers = HashMap::new();
    let existing = std::mem::take(&mut rules.headers);
    for (key, raw_value) in existing {
        let value = raw_value.trim();
        if value.is_empty() {
            continue;
        }
        if handle_rewrite_key(rules, &key, value) {
            continue;
        }
        headers.insert(key.trim().to_string(), value.to_string());
    }
    rules.headers = headers;
}

fn extract_string_array(value: Option<serde_json::value::Value>) -> Vec<String> {
    match value {
        Some(json) => match json {
            serde_json::Value::Array(items) => items
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            serde_json::Value::String(s) => vec![s],
            _ => Vec::new(),
        },
        None => Vec::new(),
    }
}

fn extract_host_from_uri(uri: &str) -> Option<String> {
    rsipstack::sip::Uri::try_from(uri)
        .ok()
        .map(|parsed| parsed.host_with_port.host.to_string())
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.iter().any(|existing| existing == &value) {
        list.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sip_trunk_codecs_default_on_and_opt_out() {
        use serde_json::json;
        let hd = vec!["opus", "g722", "pcmu", "pcma", "g729"];
        // Default ON: no metadata / no media key / unrelated keys → HD list.
        assert_eq!(sip_trunk_codecs(None), hd);
        assert_eq!(sip_trunk_codecs(Some(&json!({}))), hd);
        assert_eq!(sip_trunk_codecs(Some(&json!({"media": {}}))), hd);
        assert_eq!(sip_trunk_codecs(Some(&json!({"recording": {"mode": "all"}}))), hd);
        // Opt OUT → empty, so routing falls back to compiled defaults.
        assert!(sip_trunk_codecs(Some(&json!({"media": {"hd_disabled": true}}))).is_empty());
        // Explicit custom list overrides the default set.
        assert_eq!(
            sip_trunk_codecs(Some(&json!({"media": {"codecs": ["g722", "pcmu"]}}))),
            vec!["g722", "pcmu"]
        );
        // hd_disabled wins over any codecs list.
        assert!(
            sip_trunk_codecs(Some(
                &json!({"media": {"hd_disabled": true, "codecs": ["opus"]}})
            ))
            .is_empty()
        );
    }

    #[test]
    fn slugify_queue_name_strips_whitespace() {
        assert_eq!(
            queue_utils::slugify_queue_name("  Sales Support  "),
            "sales-support"
        );
        assert_eq!(queue_utils::slugify_queue_name("UPPER_case"), "upper-case");
        assert_eq!(queue_utils::slugify_queue_name("..special??"), "special");
    }

    #[test]
    fn route_metadata_sets_queue_fields() {
        let mut action = RouteAction::default();
        let meta = RouteMetadataAction {
            target_type: Some("queue".to_string()),
            queue_file: Some("queues/support.toml".to_string()),
            voicemail_extension: None,
            ivr_file: None,
        };
        apply_route_metadata(&mut action, meta);
        assert_eq!(action.queue.as_deref(), Some("queues/support.toml"));
    }

    #[test]
    fn route_metadata_sets_voicemail_fields() {
        let mut action = RouteAction::default();
        let meta = RouteMetadataAction {
            target_type: Some("voicemail".to_string()),
            queue_file: None,
            voicemail_extension: Some("1001".to_string()),
            ivr_file: None,
        };
        apply_route_metadata(&mut action, meta);
        assert_eq!(action.app.as_deref(), Some("voicemail"));
        let params = action.app_params.unwrap();
        assert_eq!(params["extension"], "1001");
    }

    #[test]
    fn route_metadata_sets_ivr_fields() {
        let mut action = RouteAction::default();
        let meta = RouteMetadataAction {
            target_type: Some("ivr".to_string()),
            queue_file: None,
            voicemail_extension: None,
            ivr_file: Some("config/ivr/main.toml".to_string()),
        };
        apply_route_metadata(&mut action, meta);
        assert_eq!(action.app.as_deref(), Some("ivr"));
        let params = action.app_params.unwrap();
        assert_eq!(params["file"], "config/ivr/main.toml");
    }

    #[test]
    fn acl_module_presence_check() {
        let acl_enabled = |modules: Option<Vec<String>>| -> bool {
            modules.as_deref().unwrap_or(&[]).iter().any(|m| m == "acl")
        };

        assert!(!acl_enabled(None), "None modules → acl not enabled");
        assert!(
            !acl_enabled(Some(vec!["recording".to_string()])),
            "modules without acl → not enabled"
        );
        assert!(
            acl_enabled(Some(vec!["acl".to_string(), "recording".to_string()])),
            "modules with acl → enabled"
        );
        assert!(
            acl_enabled(Some(vec!["acl".to_string()])),
            "only acl → enabled"
        );
    }

    // -----------------------------------------------------------------
    // PR 2B — kind-aware file-based trunk loader (Phase 8b).
    //
    // The loader accepts three TOML shapes per file:
    //   1. Legacy routing-format: `[trunks.<name>] dest=...` — unchanged.
    //   2. Kind-aware nested: `[[trunk]] kind="sip" [kind_config] ...`.
    //   3. Kind-aware legacy SIP: `[[trunk]] name="..." sip_server="..."`
    //      with top-level fields and no `[kind_config]`. Documented as
    //      supported for one release cycle; new files should use #2.
    // -----------------------------------------------------------------

    fn write_trunk_file(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustpbx_pr2b_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn load_legacy_routing_format_unchanged() {
        // The historical `[trunks.<name>]` shape still works.
        let path = write_trunk_file(
            "legacy.toml",
            r#"[trunks.legacy_pstn]
dest = "sip:1.2.3.4:5060"
direction = "bidirectional"
transport = "udp"
inbound_hosts = ["1.2.3.4"]
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should succeed");
        let trunk = trunks.get("legacy_pstn").expect("trunk loaded");
        assert_eq!(trunk.dest, "sip:1.2.3.4:5060");
        assert_eq!(trunk.transport.as_deref(), Some("udp"));
    }

    #[test]
    fn load_kind_aware_nested_sip_folds_into_routing() {
        crate::models::kind_schemas::register_builtins();
        let path = write_trunk_file(
            "nested.toml",
            r#"[[trunk]]
name = "nested_sip"
kind = "sip"
is_active = true
direction = "outbound"

[trunk.kind_config]
sip_server = "5.6.7.8:5060"
sip_transport = "udp"
auth_username = "alice"
auth_password = "s3cret"
register_enabled = true
register_expires = 600
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should succeed");
        let trunk = trunks.get("nested_sip").expect("trunk loaded");
        assert_eq!(trunk.dest, "5.6.7.8:5060");
        assert_eq!(trunk.username.as_deref(), Some("alice"));
        assert_eq!(trunk.password.as_deref(), Some("s3cret"));
        assert_eq!(trunk.register_enabled, Some(true));
        assert_eq!(trunk.register_expires, Some(600));
        assert_eq!(trunk.disabled, Some(false));
    }

    #[test]
    fn load_kind_aware_legacy_sip_fields_fold() {
        // Back-compat: kind absent (defaults to "sip"), top-level SIP fields.
        crate::models::kind_schemas::register_builtins();
        let path = write_trunk_file(
            "legacy_sip.toml",
            r#"[[trunk]]
name = "legacy_sip"
is_active = true
sip_server = "9.9.9.9:5060"
sip_transport = "tcp"
auth_username = "bob"
register_enabled = false
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should succeed");
        let trunk = trunks.get("legacy_sip").expect("trunk loaded");
        assert_eq!(trunk.dest, "9.9.9.9:5060");
        assert_eq!(trunk.transport.as_deref(), Some("tcp"));
        assert_eq!(trunk.username.as_deref(), Some("bob"));
    }

    #[test]
    fn load_kind_aware_unknown_kind_is_skipped() {
        crate::models::kind_schemas::register_builtins();
        let path = write_trunk_file(
            "unknown.toml",
            r#"[[trunk]]
name = "frobnicator"
kind = "frobnicate"

[trunk.kind_config]
foo = "bar"
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should still return Ok");
        assert!(
            trunks.get("frobnicator").is_none(),
            "unknown kind must be skipped, not added to routing map"
        );
    }

    #[test]
    fn load_kind_aware_webrtc_lands_in_routing_map_with_kind_discriminator() {
        // Phase 7 (PR 4): webrtc trunks now land in the routing map so the
        // matcher's Forward arm can detect `kind="webrtc"` and short-circuit
        // into the bridge dispatcher (see `proxy/routing/matcher.rs`).
        crate::proxy::bridge::signaling::register_builtins();
        crate::models::kind_schemas::register_builtins();
        let path = write_trunk_file(
            "webrtc_ok.toml",
            r#"[[trunk]]
name = "pipecat_bot"
kind = "webrtc"
is_active = true
direction = "outbound"

[trunk.kind_config]
signaling = "http_json"
endpoint_url = "http://127.0.0.1:7860/api/offer"
audio_codec = "opus"

[trunk.kind_config.protocol]
request_body_template = '{"sdp":"{offer_sdp}","type":"offer"}'
response_answer_path = "$.sdp"
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should succeed");
        let trunk = trunks
            .get("pipecat_bot")
            .expect("webrtc trunk should appear in routing map for Phase 7 dispatch");
        assert_eq!(trunk.kind, "webrtc");
        assert_eq!(trunk.dest, "http://127.0.0.1:7860/api/offer");
        let kc = trunk.kind_config.as_ref().expect("kind_config preserved");
        assert_eq!(kc.get("signaling").and_then(|v| v.as_str()), Some("http_json"));
    }

    #[test]
    fn load_kind_aware_webrtc_missing_signaling_is_rejected() {
        crate::models::kind_schemas::register_builtins();
        // `signaling = ""` fails WebRtcTrunkConfig::validate.
        let path = write_trunk_file(
            "webrtc_bad.toml",
            r#"[[trunk]]
name = "bad_bot"
kind = "webrtc"

[trunk.kind_config]
signaling = ""
endpoint_url = "http://127.0.0.1:7860/api/offer"
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should not error, just skip");
        assert!(trunks.get("bad_bot").is_none(), "invalid webrtc trunk must be skipped");
    }

    #[test]
    fn load_kind_aware_non_sip_requires_kind_config() {
        crate::models::kind_schemas::register_builtins();
        let path = write_trunk_file(
            "missing_config.toml",
            r#"[[trunk]]
name = "no_config"
kind = "webrtc"
"#,
        );
        let (trunks, _files) = load_trunks_from_files(&[path.to_string_lossy().to_string()])
            .expect("load should return Ok and skip the entry");
        assert!(trunks.get("no_config").is_none());
    }

    #[test]
    fn trunk_with_inbound_hosts_detected() {
        let mut trunks: HashMap<String, TrunkConfig> = HashMap::new();
        let no_hosts = TrunkConfig {
            dest: "sip:192.0.2.1".to_string(),
            inbound_hosts: vec![],
            ..Default::default()
        };
        let with_hosts = TrunkConfig {
            dest: "sip:192.0.2.2".to_string(),
            inbound_hosts: vec!["203.0.113.1".to_string()],
            ..Default::default()
        };

        trunks.insert("no-hosts".to_string(), no_hosts);
        assert!(
            !trunks.values().any(|t| !t.inbound_hosts.is_empty()),
            "no trunk has inbound_hosts"
        );

        trunks.insert("with-hosts".to_string(), with_hosts);
        assert!(
            trunks.values().any(|t| !t.inbound_hosts.is_empty()),
            "one trunk has inbound_hosts — warning should fire"
        );
    }
}
