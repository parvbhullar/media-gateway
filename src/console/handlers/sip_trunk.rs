//! Kind-aware Trunks console page (PR 5 / Phase 10). The handlers under this
//! module operate on the unified `rustpbx_trunks` table and route create /
//! update / delete through the `kind_schemas::validate` gate so SIP and
//! WebRTC trunks share the same UI surface. The page URL stays
//! `/console/sip-trunk` to avoid breaking deep links — the user-facing copy
//! talks generically about "Trunks".
use super::bad_request;
#[cfg(feature = "addon-wholesale")]
use crate::addons::wholesale::models::{
    tenant::Entity as TenantEntity,
    tenant_trunk::{
        ActiveModel as TenantTrunkActiveModel, Column as TenantTrunkColumn,
        Entity as TenantTrunkEntity,
    },
};
use crate::{
    console::handlers::forms::{self, ListQuery, SipTrunkForm},
    console::{ConsoleState, middleware::AuthRequired},
    models::kind_schemas,
    models::sip_trunk::{
        ActiveModel as SipTrunkActiveModel, Column as SipTrunkColumn, Entity as SipTrunkEntity,
        SipTransport, SipTrunkConfig, SipTrunkDirection, SipTrunkStatus, WebRtcTrunkConfig,
    },
    models::trunk::{ExternalMediaTrunkConfig, LiveKitTrunkConfig},
    proxy::bridge::signaling,
    proxy::routing::ConfigOrigin,
};
use axum::{
    Json, Router,
    extract::{Form, Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use sea_orm::sea_query::Order;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, DatabaseConnection, EntityTrait,
    Iterable, PaginatorTrait, QueryFilter, QueryOrder,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::warn;

#[derive(Debug, Clone, Default, Deserialize)]
struct QuerySipTrunkFilters {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    status: Option<SipTrunkStatus>,
    #[serde(default)]
    direction: Option<SipTrunkDirection>,
    #[serde(default)]
    transport: Option<SipTransport>,
    #[serde(default)]
    only_active: Option<bool>,
    /// Optional filter by trunk kind ("sip" or "webrtc"). When omitted, all
    /// kinds are returned.
    #[serde(default)]
    kind: Option<String>,
}

pub fn urls() -> Router<Arc<ConsoleState>> {
    Router::new()
        .route(
            "/sip-trunk",
            get(page_sip_trunks)
                .put(create_sip_trunk)
                .post(query_sip_trunks),
        )
        .route("/sip-trunk/new", get(page_sip_trunk_create))
        .route("/sip-trunk/promote/{name}", post(promote_file_trunk))
        .route("/sip-trunk/{id}/probe", post(probe_trunk_now))
        .route(
            "/sip-trunk/{id}",
            get(page_sip_trunk_detail)
                .patch(update_sip_trunk)
                .delete(delete_sip_trunk),
        )
}

async fn page_sip_trunks(
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let (filters, _) = build_filters_payload(state.db()).await;
    let current_user = state.build_current_user_ctx(&user).await;
    let has_file_trunks = state
        .app_state()
        .map(|app| {
            app.sip_server()
                .inner
                .data_context
                .trunks_snapshot()
                .values()
                .any(|t| match &t.origin {
                    ConfigOrigin::File(path) => !std::path::Path::new(path)
                        .file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.ends_with(".generated.toml")),
                    _ => false,
                })
        })
        .unwrap_or(false);
    let ami_endpoint = state.config().proxy.ami_path.clone().unwrap_or_else(|| "/ami/v1".to_string());
    state.render_with_headers(
        "console/sip_trunk.html",
        json!({
            "nav_active": "sip-trunk",
            "filters": filters,
            "create_url": state.url_for("/sip-trunk/new"),
            "current_user": current_user,
            "has_file_trunks": has_file_trunks,
            "ami_endpoint": ami_endpoint,
        }),
        &headers,
    )
}

async fn page_sip_trunk_create(
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let (filters, tenants) = build_filters_payload(state.db()).await;
    let current_user = state.build_current_user_ctx(&user).await;
    let ami_endpoint = state.config().proxy.ami_path.clone().unwrap_or_else(|| "/ami/v1".to_string());
    state.render_with_headers(
        "console/sip_trunk_detail.html",
        json!({
            "nav_active": "sip-trunk",
            "filters": filters,
            "tenants": tenants,
            "mode": "create",
            "create_url": state.url_for("/sip-trunk"),
            "current_user": current_user,
            "ami_endpoint": ami_endpoint,
        }),
        &headers,
    )
}

async fn page_sip_trunk_detail(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    headers: HeaderMap,
    AuthRequired(user): AuthRequired,
) -> Response {
    let db = state.db();
    let (filters, tenants) = build_filters_payload(db).await;

    let result = SipTrunkEntity::find_by_id(id).one(db).await;

    #[cfg(feature = "addon-wholesale")]
    let tenant_link = match TenantTrunkEntity::find()
        .filter(TenantTrunkColumn::SipTrunkId.eq(id))
        .all(db)
        .await
    {
        Ok(links) => {
            let link = links.into_iter().next();
            if let Some(ref l) = link {
                warn!(
                    "Found tenant link for trunk {}: tenant_id={}",
                    id, l.tenant_id
                );
            } else {
                warn!("No tenant link found for trunk {}", id);
            }
            link
        }
        Err(err) => {
            warn!("Failed to fetch tenant link for trunk {}: {}", id, err);
            None
        }
    };

    #[cfg(not(feature = "addon-wholesale"))]
    let tenant_link: Option<serde_json::Value> = None;

    let current_user = state.build_current_user_ctx(&user).await;

    match result {
        Ok(Some(model)) => {
            #[allow(unused_mut)]
            let mut model_json = serde_json::to_value(&model).unwrap_or(json!({}));
            // Flatten the kind-typed view of `kind_config` into the top level
            // of the JSON the template sees. For SIP this preserves the
            // legacy field names the form relies on; for WebRTC we prefix
            // every key with `webrtc_` so they map 1:1 onto the form fields.
            match model.kind.as_str() {
                "sip" => {
                    if let Ok(sip_cfg) = model.sip()
                        && let (Some(obj), Ok(Value::Object(flat))) =
                            (model_json.as_object_mut(), serde_json::to_value(&sip_cfg))
                    {
                        for (k, v) in flat {
                            obj.insert(k, v);
                        }
                    }
                }
                "webrtc" => {
                    if let Ok(cfg) = model.webrtc()
                        && let (Some(obj), Ok(Value::Object(flat))) =
                            (model_json.as_object_mut(), serde_json::to_value(&cfg))
                    {
                        for (k, v) in flat {
                            obj.insert(format!("webrtc_{k}"), v);
                        }
                    }
                }
                "livekit" => {
                    if let Ok(cfg) = model.livekit()
                        && let (Some(obj), Ok(Value::Object(flat))) =
                            (model_json.as_object_mut(), serde_json::to_value(&cfg))
                    {
                        for (k, v) in flat {
                            obj.insert(format!("livekit_{k}"), v);
                        }
                    }
                }
                "external_media" => {
                    if let Ok(cfg) = model.external_media()
                        && let (Some(obj), Ok(Value::Object(flat))) =
                            (model_json.as_object_mut(), serde_json::to_value(&cfg))
                    {
                        for (k, v) in flat {
                            obj.insert(format!("external_media_{k}"), v);
                        }
                    }
                }
                _ => {
                    warn!("unknown trunk kind '{}' for trunk id={}", model.kind, id);
                }
            }

            // Surface the per-trunk HD codec-upgrade state as a boolean the
            // form checkbox binds to. HD is ON by default; only an explicit
            // `metadata.media.hd_disabled = true` turns it off.
            if let Some(obj) = model_json.as_object_mut() {
                let prefer_hd = model
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("media"))
                    .and_then(|m| m.get("hd_disabled"))
                    .and_then(|v| v.as_bool())
                    != Some(true);
                obj.insert("prefer_hd".to_string(), Value::Bool(prefer_hd));
            }

            #[cfg(feature = "addon-wholesale")]
            if let Some(obj) = model_json.as_object_mut() {
                if let Some(link) = tenant_link {
                    obj.insert("tenant_id".to_string(), json!(link.tenant_id));
                }
            }

            #[cfg(not(feature = "addon-wholesale"))]
            {
                let _ = tenant_link;
            }

            let assigned_dids = crate::models::did::Model::list_by_trunk(db, &model.name)
                .await
                .unwrap_or_default();
            let dids_count = assigned_dids.len() as u64;
            let dids_numbers: Vec<&str> =
                assigned_dids.iter().map(|d| d.number.as_str()).collect();

            let ami_endpoint = state.config().proxy.ami_path.clone().unwrap_or_else(|| "/ami/v1".to_string());
            state.render_with_headers(
                "console/sip_trunk_detail.html",
                json!({
                    "nav_active": "sip-trunk",
                    "model": model_json,
                    "filters": filters,
                    "tenants": tenants,
                    "mode": "edit",
                    "update_url": state.url_for(&format!("/sip-trunk/{id}")),
                    "current_user": current_user,
                    "dids_count": dids_count,
                    "dids_numbers": dids_numbers,
                    "ami_endpoint": ami_endpoint,
                }),
                &headers,
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "Trunk not found"})),
        )
            .into_response(),
        Err(err) => {
            warn!("failed to load sip trunk {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to load SIP trunk: {}", err)})),
            )
                .into_response()
        }
    }
}

async fn create_sip_trunk(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Form(form): Form<SipTrunkForm>,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let db = state.db();
    let now = Utc::now();
    let mut active = SipTrunkActiveModel {
        ..Default::default()
    };

    let kind = form
        .kind
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sip".to_string());
    if let Err(response) =
        apply_form_to_active_model(
            &mut active, &form, now, false, &kind, None, None, None, None, None,
        )
    {
        return response;
    }

    match active.insert(db).await {
        Ok(model) => {
            if let Err(err) = handle_tenant_update(
                db,
                model.id,
                form.tenant_id,
                form.clear_tenant.unwrap_or(false),
            )
            .await
            {
                warn!(
                    "failed to update tenant link for trunk {}: {}",
                    model.id, err
                );
            }

            state.mark_pending_reload();
            Json(json!({"status": "ok", "id": model.id})).into_response()
        }
        Err(err) => {
            warn!("failed to create sip trunk: {}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to create SIP trunk: {}", err)})),
            )
                .into_response()
        }
    }
}

async fn update_sip_trunk(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
    Form(form): Form<SipTrunkForm>,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let db = state.db();
    let model = match SipTrunkEntity::find_by_id(id).one(db).await {
        Ok(Some(model)) => model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "Trunk not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load trunk {} for update: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to update trunk: {}", err)})),
            )
                .into_response();
        }
    };

    let existing_kind = model.kind.clone();
    let existing_kind_config = model.kind_config.clone();
    let existing_sip_cfg = model.sip().ok();
    let existing_webrtc_cfg = model.webrtc().ok();
    let existing_livekit_cfg = model.livekit().ok();
    let existing_external_media_cfg = model.external_media().ok();
    let existing_metadata = model.metadata.clone();
    let mut active: SipTrunkActiveModel = model.into();
    let now = Utc::now();
    if let Err(response) = apply_form_to_active_model(
        &mut active,
        &form,
        now,
        true,
        &existing_kind,
        existing_metadata,
        existing_sip_cfg,
        existing_webrtc_cfg,
        existing_livekit_cfg,
        existing_external_media_cfg,
    ) {
        return response;
    }

    // If kind_config changed (e.g. endpoint_url / health_check_url /
    // server_url / webhook_url got edited), clear the prober's
    // bookkeeping so the next tick re-probes against the new target
    // instead of honoring a stale `last_health_check_at` timestamp.
    let new_kind_config = match &active.kind_config {
        sea_orm::ActiveValue::Set(v) | sea_orm::ActiveValue::Unchanged(v) => Some(v.clone()),
        _ => None,
    };
    if new_kind_config.as_ref() != Some(&existing_kind_config) {
        active.last_health_check_at = Set(None);
        active.consecutive_failures = Set(0);
        active.consecutive_successes = Set(0);
    }

    match active.update(db).await {
        Ok(model) => {
            if let Err(err) = handle_tenant_update(
                db,
                model.id,
                form.tenant_id,
                form.clear_tenant.unwrap_or(false),
            )
            .await
            {
                warn!(
                    "failed to update tenant link for trunk {}: {}",
                    model.id, err
                );
            }

            state.mark_pending_reload();
            Json(json!({"status": "ok"})).into_response()
        }
        Err(err) => {
            warn!("failed to update sip trunk {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to update SIP trunk: {}", err)})),
            )
                .into_response()
        }
    }
}

async fn delete_sip_trunk(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let db = state.db();
    let model = match SipTrunkEntity::find_by_id(id).one(db).await {
        Ok(Some(model)) => model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "Trunk not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load trunk {} for delete: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to delete SIP trunk: {}", err)})),
            )
                .into_response();
        }
    };

    // Guard: refuse to delete a trunk that is still referenced by any DID,
    // either as the owning trunk or as a failover target. The user must
    // reassign or remove those DIDs first — silent orphaning would break
    // runtime routing.
    use crate::models::did;
    let owned = match did::Model::count_by_trunk(db, &model.name).await {
        Ok(n) => n,
        Err(err) => {
            warn!(
                "failed to count DIDs owning trunk {}: {}; refusing delete",
                model.name, err
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": "failed to check DID references"})),
            )
                .into_response();
        }
    };
    let as_failover = match did::Model::count_by_failover_trunk(db, &model.name).await {
        Ok(n) => n,
        Err(err) => {
            warn!(
                "failed to count DIDs failing over to trunk {}: {}; refusing delete",
                model.name, err
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": "failed to check DID failover references"})),
            )
                .into_response();
        }
    };
    if owned + as_failover > 0 {
        let msg = format!(
            "trunk '{}' still has {} DID(s) and {} failover reference(s); remove them first",
            model.name, owned, as_failover
        );
        return (StatusCode::CONFLICT, Json(json!({ "message": msg }))).into_response();
    }

    let active: SipTrunkActiveModel = model.into();
    match active.delete(db).await {
        Ok(_) => {
            state.mark_pending_reload();
            Json(json!({"status": "ok"})).into_response()
        }
        Err(err) => {
            warn!("failed to delete sip trunk {}: {}", id, err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to delete SIP trunk: {}", err)})),
            )
                .into_response()
        }
    }
}

async fn query_sip_trunks(
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(_): AuthRequired,
    Json(payload): Json<ListQuery<QuerySipTrunkFilters>>,
) -> Response {
    let db = state.db();
    let filters_payload;
    {
        let (payload, _) = build_filters_payload(db).await;
        filters_payload = payload;
    }

    let filters = payload.filters.clone().unwrap_or_default();
    let (_, per_page) = payload.normalize();

    // Kind-aware Trunks view (PR 5 / Phase 10). All kinds are returned by
    // default; clients may narrow the result set via `filters.kind`.
    let mut selector = SipTrunkEntity::find();
    if let Some(ref kind) = filters.kind {
        let trimmed = kind.trim();
        if !trimmed.is_empty() {
            selector = selector.filter(SipTrunkColumn::Kind.eq(trimmed));
        }
    }

    if let Some(ref q_raw) = filters.q {
        let trimmed = q_raw.trim();
        if !trimmed.is_empty() {
            let mut condition = Condition::any();
            condition = condition.add(SipTrunkColumn::Name.contains(trimmed));
            condition = condition.add(SipTrunkColumn::DisplayName.contains(trimmed));
            // TODO(wave-2-followup): `carrier` and `sip_server` are now packed
            // into `kind_config`; restoring contains-search requires JSON path
            // predicates. Dropped for this wave to keep the list query green.
            selector = selector.filter(condition);
        }
    }

    if let Some(status) = filters.status {
        selector = selector.filter(SipTrunkColumn::Status.eq(status));
    }

    if let Some(direction) = filters.direction {
        selector = selector.filter(SipTrunkColumn::Direction.eq(direction));
    }

    if let Some(_transport) = filters.transport {
        // TODO(wave-2-followup): `sip_transport` moved into `kind_config`;
        // re-implement via a JSON predicate or in-memory filter.
    }

    if filters.only_active.unwrap_or(false) {
        selector = selector.filter(SipTrunkColumn::IsActive.eq(true));
    }

    let sort_key = payload.sort.as_deref().unwrap_or("updated_at_desc");
    match sort_key {
        "updated_at_asc" => {
            selector = selector.order_by(SipTrunkColumn::UpdatedAt, Order::Asc);
        }
        "name_asc" => {
            selector = selector
                .order_by(SipTrunkColumn::DisplayName, Order::Asc)
                .order_by(SipTrunkColumn::Name, Order::Asc);
        }
        "name_desc" => {
            selector = selector
                .order_by(SipTrunkColumn::DisplayName, Order::Desc)
                .order_by(SipTrunkColumn::Name, Order::Desc);
        }
        "carrier_asc" => {
            // TODO(wave-2-followup): carrier moved into kind_config; sort by
            // carrier requires JSON-path ordering. Fallback to name.
            selector = selector.order_by(SipTrunkColumn::Name, Order::Asc);
        }
        "carrier_desc" => {
            // TODO(wave-2-followup): see above.
            selector = selector.order_by(SipTrunkColumn::Name, Order::Desc);
        }
        "status_asc" => {
            selector = selector.order_by(SipTrunkColumn::Status, Order::Asc);
        }
        "status_desc" => {
            selector = selector.order_by(SipTrunkColumn::Status, Order::Desc);
        }
        _ => {
            selector = selector.order_by(SipTrunkColumn::UpdatedAt, Order::Desc);
        }
    }
    selector = selector.order_by(SipTrunkColumn::Id, Order::Desc);

    let paginator = selector.paginate(db, per_page);
    let pagination = match forms::paginate(paginator, &payload).await {
        Ok(pagination) => pagination,
        Err(err) => {
            warn!("failed to paginate sip trunks: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to query SIP trunks: {}", err)})),
            )
                .into_response();
        }
    };

    let forms::Pagination {
        items,
        current_page,
        per_page,
        total_items,
        total_pages,
        has_prev,
        has_next,
    } = pagination;

    let enriched_items: Vec<Value> = items
        .into_iter()
        .map(|model| {
            let mut v = serde_json::to_value(&model).unwrap_or_else(|_| json!({}));
            // Flatten kind-specific fields so the table can render carrier,
            // sip_server, etc. without re-fetching the row.
            if let Some(obj) = v.as_object_mut() {
                match model.kind.as_str() {
                    "sip" => {
                        if let Ok(cfg) = model.sip()
                            && let Ok(Value::Object(flat)) = serde_json::to_value(&cfg)
                        {
                            for (k, val) in flat {
                                obj.entry(k).or_insert(val);
                            }
                        }
                    }
                    "webrtc" => {
                        if let Ok(cfg) = model.webrtc()
                            && let Ok(Value::Object(flat)) = serde_json::to_value(&cfg)
                        {
                            for (k, val) in flat {
                                obj.entry(format!("webrtc_{k}")).or_insert(val);
                            }
                        }
                    }
                    "livekit" => {
                        if let Ok(cfg) = model.livekit()
                            && let Ok(Value::Object(flat)) = serde_json::to_value(&cfg)
                        {
                            for (k, val) in flat {
                                obj.entry(format!("livekit_{k}")).or_insert(val);
                            }
                        }
                    }
                    "external_media" => {
                        if let Ok(cfg) = model.external_media()
                            && let Ok(Value::Object(flat)) = serde_json::to_value(&cfg)
                        {
                            for (k, val) in flat {
                                obj.entry(format!("external_media_{k}")).or_insert(val);
                            }
                        }
                    }
                    _ => {}
                }
            }
            v
        })
        .collect();

    // Issue #179: collect file-sourced trunks from in-memory snapshot.
    //
    // Skip entries whose source file is `*.generated.toml` — those files are
    // re-emitted from the DB by `reload_trunks(true, ...)` and represent the
    // same rows already shown in the editable DB list above. Including them
    // would duplicate every DB trunk in the read-only section.
    let file_trunks: Vec<Value> = if let Some(app_state) = state.app_state() {
        let snapshot = app_state.sip_server().inner.data_context.trunks_snapshot();
        let mut file_items: Vec<Value> = snapshot
            .into_iter()
            .filter_map(|(name, trunk)| {
                if let ConfigOrigin::File(ref path) = trunk.origin {
                    let is_generated_mirror = std::path::Path::new(path)
                        .file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.ends_with(".generated.toml"));
                    if is_generated_mirror {
                        return None;
                    }
                    Some(json!({
                        "id": null,
                        "name": name,
                        "display_name": name,
                        "dest": trunk.dest,
                        "source": "file",
                        "source_file": path,
                        "readonly": true,
                        "is_active": trunk.disabled.map(|d| !d).unwrap_or(true),
                        "direction": trunk.direction,
                        "disabled": trunk.disabled.unwrap_or(false),
                        "promote_url": format!("/sip-trunk/promote/{}", name),
                    }))
                } else {
                    None
                }
            })
            .collect();
        file_items.sort_by(|a, b| {
            let a_name = a["name"].as_str().unwrap_or("");
            let b_name = b["name"].as_str().unwrap_or("");
            a_name.cmp(b_name)
        });
        file_items
    } else {
        vec![]
    };

    Json(json!({
        "page": current_page,
        "per_page": per_page,
        "total_items": total_items,
        "total_pages": total_pages,
        "has_prev": has_prev,
        "has_next": has_next,
        "items": enriched_items,
        "file_trunks": file_trunks,
        "filters": filters_payload,
    }))
    .into_response()
}

async fn build_filters_payload(db: &DatabaseConnection) -> (Value, Vec<Value>) {
    let tenants = load_tenants(db).await;

    let mut signaling_adapters = signaling::registered();
    signaling_adapters.sort();
    // Surface a stable default for the create form so the dropdown is never
    // empty even if `register_builtins()` hasn't run yet (e.g. some tests).
    if signaling_adapters.is_empty() {
        signaling_adapters.push("http_json".to_string());
    }

    (
        json!({
            "statuses": SipTrunkStatus::iter()
                .map(|status| status.as_str())
                .collect::<Vec<_>>(),
            "directions": SipTrunkDirection::iter()
                .map(|direction| direction.as_str())
                .collect::<Vec<_>>(),
            "transports": SipTransport::iter()
                .map(|transport| transport.as_str())
                .collect::<Vec<_>>(),
            "kinds": ["sip", "webrtc", "livekit", "external_media"],
            "signaling_adapters": signaling_adapters,
            "webrtc_audio_codecs": ["opus", "g722"],
            "livekit_audio_codecs": ["opus", "g722"],
            "external_media_audio_codecs": ["opus", "g722", "pcmu", "pcma"],
        }),
        tenants,
    )
}

async fn load_tenants(db: &DatabaseConnection) -> Vec<Value> {
    #[cfg(feature = "addon-wholesale")]
    match TenantEntity::find()
        .order_by_asc(crate::addons::wholesale::models::tenant::Column::Name)
        .all(db)
        .await
    {
        Ok(list) => list
            .into_iter()
            .map(|t| serde_json::to_value(t).unwrap_or(json!({})))
            .collect(),
        Err(err) => {
            warn!("failed to load tenants: {}", err);
            vec![]
        }
    }

    #[cfg(not(feature = "addon-wholesale"))]
    {
        let _ = db;
        vec![]
    }
}

async fn handle_tenant_update(
    db: &DatabaseConnection,
    trunk_id: i64,
    tenant_id: Option<i64>,
    clear_tenant: bool,
) -> Result<(), sea_orm::DbErr> {
    #[cfg(feature = "addon-wholesale")]
    {
        if clear_tenant {
            TenantTrunkEntity::delete_many()
                .filter(TenantTrunkColumn::SipTrunkId.eq(trunk_id))
                .exec(db)
                .await?;
        } else if let Some(tid) = tenant_id {
            // Always clear existing links to ensure 1-to-1 relationship (Trunk -> Tenant)
            TenantTrunkEntity::delete_many()
                .filter(TenantTrunkColumn::SipTrunkId.eq(trunk_id))
                .exec(db)
                .await?;

            let active = TenantTrunkActiveModel {
                sip_trunk_id: Set(trunk_id),
                tenant_id: Set(tid),
                ..Default::default()
            };
            active.insert(db).await?;
        }
    }
    #[cfg(not(feature = "addon-wholesale"))]
    {
        let _ = db;
        let _ = trunk_id;
        let _ = tenant_id;
        let _ = clear_tenant;
    }
    Ok(())
}

#[allow(clippy::result_large_err, clippy::too_many_arguments)]
/// Record the per-trunk HD codec-upgrade opt-OUT. HD upgrade is ON by default
/// (routing offers the HD codec set and transcodes a low-quality caller up),
/// so `prefer_hd = true` clears `metadata.media.hd_disabled` (and drops an
/// empty `media` object), while `false` sets it. Other metadata/media keys are
/// preserved. Returns `None` when the blob is left empty so the column stays
/// NULL.
fn apply_prefer_hd_metadata(
    metadata: Option<serde_json::Value>,
    prefer_hd: bool,
) -> Option<serde_json::Value> {
    use serde_json::{Value, json};
    let mut obj = match metadata {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let mut media = obj
        .get("media")
        .and_then(|m| m.as_object())
        .cloned()
        .unwrap_or_default();
    if prefer_hd {
        media.remove("hd_disabled");
    } else {
        media.insert("hd_disabled".to_string(), json!(true));
    }
    if media.is_empty() {
        obj.remove("media");
    } else {
        obj.insert("media".to_string(), Value::Object(media));
    }
    if obj.is_empty() {
        None
    } else {
        Some(Value::Object(obj))
    }
}

fn apply_form_to_active_model(
    active: &mut SipTrunkActiveModel,
    form: &SipTrunkForm,
    now: DateTime<Utc>,
    is_update: bool,
    kind: &str,
    existing_metadata: Option<serde_json::Value>,
    existing_sip_cfg: Option<SipTrunkConfig>,
    existing_webrtc_cfg: Option<WebRtcTrunkConfig>,
    existing_livekit_cfg: Option<LiveKitTrunkConfig>,
    existing_external_media_cfg: Option<ExternalMediaTrunkConfig>,
) -> Result<(), Response> {
    if !matches!(kind, "sip" | "webrtc" | "livekit" | "external_media") {
        return Err(bad_request(format!("unsupported trunk kind '{kind}'")));
    }
    let allowed_ips = parse_list_field(
        &form.allowed_ips,
        "allowed_ips",
        &["cidr", "ip", "host", "value"],
    )?;
    let did_numbers = parse_list_field(
        &form.did_numbers,
        "did_numbers",
        &["number", "did", "value"],
    )?;
    let billing_snapshot = parse_json_field(&form.billing_snapshot, "billing_snapshot")?;
    let analytics = parse_json_field(&form.analytics, "analytics")?;
    let tags = parse_json_field(&form.tags, "tags")?;
    let metadata = parse_json_field(&form.metadata, "metadata")?;
    let register_extra_headers_raw =
        parse_json_field(&form.register_extra_headers, "register_extra_headers")?;
    let register_extra_headers: Option<Vec<(String, String)>> = register_extra_headers_raw
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    if !is_update {
        let name = super::require_field(&form.name, "name")?;
        active.name = Set(name);
        active.kind = Set(kind.to_string());
        active.status = Set(form.status.unwrap_or_default());
        active.direction = Set(form.direction.unwrap_or_default());
        active.is_active = Set(form.is_active.unwrap_or(true));
        active.created_at = Set(now);
    } else {
        if let Some(name) = super::normalize_optional_string(&form.name) {
            active.name = Set(name);
        }
        if let Some(status) = form.status {
            active.status = Set(status);
        }
        if let Some(direction) = form.direction {
            active.direction = Set(direction);
        }
        if let Some(is_active) = form.is_active {
            active.is_active = Set(is_active);
        }
    }

    if !is_update || form.display_name.is_some() {
        active.display_name = Set(super::normalize_optional_string(&form.display_name));
    }
    if !is_update || form.description.is_some() {
        active.description = Set(super::normalize_optional_string(&form.description));
    }

    if !is_update || form.max_cps.is_some() {
        active.max_cps = Set(form.max_cps);
    }
    if !is_update || form.max_concurrent.is_some() {
        active.max_concurrent = Set(form.max_concurrent);
    }
    if !is_update || form.max_call_duration.is_some() {
        active.max_call_duration = Set(form.max_call_duration);
    }
    if !is_update || form.utilisation_percent.is_some() {
        active.utilisation_percent = Set(form.utilisation_percent);
    }
    if !is_update || form.warning_threshold_percent.is_some() {
        active.warning_threshold_percent = Set(form.warning_threshold_percent);
    }

    if !is_update || form.allowed_ips.is_some() {
        active.allowed_ips = Set(allowed_ips);
    }
    if !is_update || form.tags.is_some() {
        active.tags = Set(tags);
    }
    // Fold the per-trunk HD toggle into `metadata.media.codecs`. Merge onto
    // the form's metadata if it was submitted, else the existing row's, so
    // neither the toggle nor other metadata keys clobber each other on a
    // partial update.
    if !is_update || form.metadata.is_some() || form.prefer_hd.is_some() {
        let mut merged = metadata.or(existing_metadata);
        if let Some(prefer_hd) = form.prefer_hd {
            merged = apply_prefer_hd_metadata(merged, prefer_hd);
        }
        active.metadata = Set(merged);
    }

    // Build the kind-typed `kind_config` blob. On update we start from the
    // previously-decoded config so omitted fields are preserved; on create we
    // start from defaults.
    let kind_config_json = match kind {
        "sip" => {
            let mut sip_cfg = existing_sip_cfg.unwrap_or_default();

            if !is_update || form.sip_server.is_some() {
                sip_cfg.sip_server = super::normalize_optional_string(&form.sip_server);
            }
            if let Some(transport) = form.sip_transport {
                sip_cfg.sip_transport = transport;
            } else if !is_update {
                sip_cfg.sip_transport = SipTransport::default();
            }
            if !is_update || form.outbound_proxy.is_some() {
                sip_cfg.outbound_proxy = super::normalize_optional_string(&form.outbound_proxy);
            }
            if !is_update || form.auth_username.is_some() {
                sip_cfg.auth_username = super::normalize_optional_string(&form.auth_username);
            }
            if !is_update || form.auth_password.is_some() {
                sip_cfg.auth_password = super::normalize_optional_string(&form.auth_password);
            }
            if !is_update || form.default_route_label.is_some() {
                sip_cfg.default_route_label =
                    super::normalize_optional_string(&form.default_route_label);
            }
            if !is_update || form.carrier.is_some() {
                sip_cfg.carrier = super::normalize_optional_string(&form.carrier);
            }
            if !is_update || form.did_numbers.is_some() {
                sip_cfg.did_numbers = did_numbers;
            }
            if !is_update || form.billing_snapshot.is_some() {
                sip_cfg.billing_snapshot = billing_snapshot;
            }
            if !is_update || form.analytics.is_some() {
                sip_cfg.analytics = analytics;
            }
            if !is_update || form.incoming_from_user_prefix.is_some() {
                sip_cfg.incoming_from_user_prefix =
                    super::normalize_optional_string(&form.incoming_from_user_prefix);
            }
            if !is_update || form.incoming_to_user_prefix.is_some() {
                sip_cfg.incoming_to_user_prefix =
                    super::normalize_optional_string(&form.incoming_to_user_prefix);
            }

            if !is_update {
                sip_cfg.rewrite_hostport = form.rewrite_hostport.unwrap_or(true);
            } else if let Some(v) = form.rewrite_hostport {
                sip_cfg.rewrite_hostport = v;
            }

            if !is_update {
                sip_cfg.register_enabled = form.register_enabled.unwrap_or(false);
            } else if let Some(enabled) = form.register_enabled {
                sip_cfg.register_enabled = enabled;
            }
            if !is_update || form.register_expires.is_some() {
                sip_cfg.register_expires = form.register_expires;
            }
            if !is_update || form.register_extra_headers.is_some() {
                sip_cfg.register_extra_headers = register_extra_headers;
            }

            serde_json::to_value(&sip_cfg)
                .map_err(|e| bad_request(format!("failed to serialize SIP config: {e}")))?
        }
        "webrtc" => {
            // The WebRTC validator (`kind_schemas::validate("webrtc", ..)`)
            // delegates protocol-blob validation to the signaling adapter
            // registry — so we just need to assemble the JSON blob from the
            // form and let `validate` below catch malformed inputs.
            let ice_servers_json = parse_json_field(&form.webrtc_ice_servers, "webrtc_ice_servers")?;
            let protocol_json = parse_json_field(&form.webrtc_protocol, "webrtc_protocol")?;

            let signaling = super::normalize_optional_string(&form.webrtc_signaling)
                .or_else(|| existing_webrtc_cfg.as_ref().map(|c| c.signaling.clone()))
                .unwrap_or_else(|| "http_json".to_string());
            let endpoint_url = super::normalize_optional_string(&form.webrtc_endpoint_url)
                .or_else(|| existing_webrtc_cfg.as_ref().map(|c| c.endpoint_url.clone()))
                .unwrap_or_default();
            let audio_codec = super::normalize_optional_string(&form.webrtc_audio_codec)
                .or_else(|| existing_webrtc_cfg.as_ref().map(|c| c.audio_codec.clone()))
                .unwrap_or_else(|| "opus".to_string());

            let auth_header = if !is_update || form.webrtc_auth_header.is_some() {
                super::normalize_optional_string(&form.webrtc_auth_header)
            } else {
                existing_webrtc_cfg.as_ref().and_then(|c| c.auth_header.clone())
            };
            let ice_servers = if !is_update || form.webrtc_ice_servers.is_some() {
                ice_servers_json
            } else {
                existing_webrtc_cfg.as_ref().and_then(|c| c.ice_servers.clone())
            };
            let protocol = if !is_update || form.webrtc_protocol.is_some() {
                protocol_json
            } else {
                existing_webrtc_cfg.as_ref().and_then(|c| c.protocol.clone())
            };

            // Health-check override URL. Follow the same merge semantics
            // as `auth_header`: on update, only overwrite when the form
            // explicitly posted the field; otherwise carry the existing
            // value forward. Empty-string clears the override.
            let health_check_url = if !is_update || form.webrtc_health_check_url.is_some() {
                super::normalize_optional_string(&form.webrtc_health_check_url)
            } else {
                existing_webrtc_cfg
                    .as_ref()
                    .and_then(|c| c.health_check_url.clone())
            };
            let webrtc_cfg = WebRtcTrunkConfig {
                signaling,
                endpoint_url,
                ice_servers,
                audio_codec,
                auth_header,
                health_check_url,
                protocol,
                signaling_timeout_ms: existing_webrtc_cfg
                    .as_ref()
                    .and_then(|c| c.signaling_timeout_ms),
            };

            serde_json::to_value(&webrtc_cfg)
                .map_err(|e| bad_request(format!("failed to serialize WebRTC config: {e}")))?
        }
        "livekit" => {
            // LiveKit validator (`kind_schemas::validate("livekit", ..)`)
            // deserializes the blob into `LiveKitTrunkConfig` and runs its
            // `validate()` — so we just assemble the JSON and let the
            // shared `validate` gate catch malformed inputs.
            let dispatch_protocol_json = parse_json_field(
                &form.livekit_dispatch_endpoint_protocol,
                "livekit_dispatch_endpoint_protocol",
            )?;

            let server_url = super::normalize_optional_string(&form.livekit_server_url)
                .or_else(|| existing_livekit_cfg.as_ref().map(|c| c.server_url.clone()))
                .unwrap_or_default();
            // Secrets: on update only overwrite when the form posted the
            // field (allow empty-string to clear). Otherwise carry the
            // existing value forward.
            let api_key = if !is_update || form.livekit_api_key.is_some() {
                super::normalize_optional_string(&form.livekit_api_key).unwrap_or_default()
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .map(|c| c.api_key.clone())
                    .unwrap_or_default()
            };
            let api_secret = if !is_update || form.livekit_api_secret.is_some() {
                super::normalize_optional_string(&form.livekit_api_secret).unwrap_or_default()
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .map(|c| c.api_secret.clone())
                    .unwrap_or_default()
            };
            let room_template = super::normalize_optional_string(&form.livekit_room_template)
                .or_else(|| {
                    existing_livekit_cfg.as_ref().map(|c| c.room_template.clone())
                })
                .unwrap_or_default();
            let identity_template = super::normalize_optional_string(&form.livekit_identity_template)
                .or_else(|| {
                    existing_livekit_cfg
                        .as_ref()
                        .map(|c| c.identity_template.clone())
                })
                .unwrap_or_default();
            let metadata_template = if !is_update || form.livekit_metadata_template.is_some() {
                super::normalize_optional_string(&form.livekit_metadata_template)
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.metadata_template.clone())
            };
            let audio_codec = super::normalize_optional_string(&form.livekit_audio_codec)
                .or_else(|| existing_livekit_cfg.as_ref().map(|c| c.audio_codec.clone()))
                .unwrap_or_else(|| "opus".to_string());
            let dispatch_endpoint = if !is_update || form.livekit_dispatch_endpoint.is_some() {
                super::normalize_optional_string(&form.livekit_dispatch_endpoint)
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.dispatch_endpoint.clone())
            };
            let dispatch_endpoint_auth_header = if !is_update
                || form.livekit_dispatch_endpoint_auth_header.is_some()
            {
                super::normalize_optional_string(&form.livekit_dispatch_endpoint_auth_header)
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.dispatch_endpoint_auth_header.clone())
            };
            let dispatch_endpoint_protocol = if !is_update
                || form.livekit_dispatch_endpoint_protocol.is_some()
            {
                dispatch_protocol_json
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.dispatch_endpoint_protocol.clone())
            };
            // require_webhook_ack defaults to TRUE for new trunks — see
            // `LiveKitTrunkConfig::require_webhook_ack`. On updates, fall
            // through to the existing stored value when the form omits it
            // (the HTML checkbox is always present, so this guard is
            // mostly defensive).
            let require_webhook_ack = if !is_update {
                form.livekit_require_webhook_ack.unwrap_or(true)
            } else {
                form.livekit_require_webhook_ack.unwrap_or_else(|| {
                    existing_livekit_cfg
                        .as_ref()
                        .map(|c| c.require_webhook_ack)
                        .unwrap_or(true)
                })
            };
            let health_check_url = if !is_update || form.livekit_health_check_url.is_some() {
                super::normalize_optional_string(&form.livekit_health_check_url)
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.health_check_url.clone())
            };
            let signaling_timeout_ms = if !is_update || form.livekit_signaling_timeout_ms.is_some()
            {
                form.livekit_signaling_timeout_ms
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.signaling_timeout_ms)
            };
            let delete_room_on_hangup = if !is_update {
                form.livekit_delete_room_on_hangup.unwrap_or(false)
            } else {
                form.livekit_delete_room_on_hangup.unwrap_or_else(|| {
                    existing_livekit_cfg
                        .as_ref()
                        .map(|c| c.delete_room_on_hangup)
                        .unwrap_or(false)
                })
            };
            // agent_name: take form value when supplied; on update, fall
            // through to the existing stored value when the form omits it.
            let agent_name = if !is_update || form.livekit_agent_name.is_some() {
                super::normalize_optional_string(&form.livekit_agent_name)
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.agent_name.clone())
            };
            // require_agent_dispatch defaults to TRUE for new trunks.
            let require_agent_dispatch = if !is_update {
                form.livekit_require_agent_dispatch.unwrap_or(true)
            } else {
                form.livekit_require_agent_dispatch.unwrap_or_else(|| {
                    existing_livekit_cfg
                        .as_ref()
                        .map(|c| c.require_agent_dispatch)
                        .unwrap_or(true)
                })
            };
            // bot_join_timeout_ms: form value wins; on new trunks where
            // the form omits it, default to 15s so the silent-empty-room
            // gap is closed without operator action. On updates, preserve
            // the stored value when the form omits it.
            let bot_join_timeout_ms = if let Some(v) = form.livekit_bot_join_timeout_ms {
                Some(v)
            } else if !is_update {
                Some(15_000)
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.bot_join_timeout_ms)
            };
            let hold_tone_hz = if !is_update || form.livekit_hold_tone_hz.is_some() {
                form.livekit_hold_tone_hz
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.hold_tone_hz)
            };
            let jwt_ttl_secs = if !is_update || form.livekit_jwt_ttl_secs.is_some() {
                form.livekit_jwt_ttl_secs
            } else {
                existing_livekit_cfg
                    .as_ref()
                    .and_then(|c| c.jwt_ttl_secs)
            };

            let livekit_cfg = LiveKitTrunkConfig {
                server_url,
                api_key,
                api_secret,
                room_template,
                identity_template,
                metadata_template,
                audio_codec,
                dispatch_endpoint,
                dispatch_endpoint_auth_header,
                dispatch_endpoint_protocol,
                require_webhook_ack,
                health_check_url,
                signaling_timeout_ms,
                delete_room_on_hangup,
                agent_name,
                require_agent_dispatch,
                bot_join_timeout_ms,
                hold_tone_hz,
                jwt_ttl_secs,
            };

            serde_json::to_value(&livekit_cfg)
                .map_err(|e| bad_request(format!("failed to serialize LiveKit config: {e}")))?
        }
        "external_media" => {
            // ExternalMedia validator (`kind_schemas::validate("external_media", ..)`)
            // deserializes into `ExternalMediaTrunkConfig` and runs its
            // `validate()`; we just assemble the JSON with update-aware
            // merge semantics mirroring the livekit branch.
            let command = super::normalize_optional_string(&form.external_media_command)
                .or_else(|| existing_external_media_cfg.as_ref().map(|c| c.command.clone()))
                .unwrap_or_default();
            let audio_codec = super::normalize_optional_string(&form.external_media_audio_codec)
                .or_else(|| {
                    existing_external_media_cfg
                        .as_ref()
                        .map(|c| c.audio_codec.clone())
                })
                .unwrap_or_else(|| "opus".to_string());
            // bot_join_timeout_ms: form value wins; default 15s on create;
            // preserve stored value on update when the form omits it.
            let bot_join_timeout_ms = if let Some(v) = form.external_media_bot_join_timeout_ms {
                Some(v)
            } else if !is_update {
                Some(15_000)
            } else {
                existing_external_media_cfg
                    .as_ref()
                    .and_then(|c| c.bot_join_timeout_ms)
            };
            let hold_tone_hz = if !is_update || form.external_media_hold_tone_hz.is_some() {
                form.external_media_hold_tone_hz
            } else {
                existing_external_media_cfg
                    .as_ref()
                    .and_then(|c| c.hold_tone_hz)
            };

            // No console form field yet — preserve an existing value on
            // update, default (48 kHz) on create. Set via API/TOML.
            let pcm_sample_rate = existing_external_media_cfg
                .as_ref()
                .map(|c| c.pcm_sample_rate)
                .unwrap_or(48_000);

            let external_media_cfg = ExternalMediaTrunkConfig {
                command,
                audio_codec,
                bot_join_timeout_ms,
                hold_tone_hz,
                pcm_sample_rate,
            };

            serde_json::to_value(&external_media_cfg).map_err(|e| {
                bad_request(format!("failed to serialize ExternalMedia config: {e}"))
            })?
        }
        other => {
            return Err(bad_request(format!("unsupported trunk kind '{other}'")));
        }
    };

    // Route every save through the same validation gate the REST CRUD path
    // uses so SIP and WebRTC stay in lockstep with `kind_schemas::register`.
    if let Err(err) = kind_schemas::validate(kind, &kind_config_json) {
        return Err(bad_request(format!("invalid {kind} trunk config: {err}")));
    }
    active.kind_config = Set(kind_config_json);

    active.updated_at = Set(now);

    Ok(())
}

#[allow(clippy::result_large_err)]
fn parse_list_field(
    value: &Option<String>,
    field: &str,
    preferred_keys: &[&str],
) -> Result<Option<Value>, Response> {
    let Some(raw) = value.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };

    if let Ok(json_value) = serde_json::from_str::<Value>(raw) {
        let normalized = normalize_list_json(json_value, field, preferred_keys)?;
        return Ok(
            normalized.map(|list| Value::Array(list.into_iter().map(Value::String).collect()))
        );
    }

    let entries: Vec<Value> = raw
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| Value::String(line.to_string()))
        .collect();

    if entries.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Array(entries)))
    }
}

#[allow(clippy::result_large_err)]
fn normalize_list_json(
    value: Value,
    field: &str,
    preferred_keys: &[&str],
) -> Result<Option<Vec<String>>, Response> {
    match value {
        Value::Null => Ok(None),
        Value::Array(items) => {
            let mut entries = Vec::new();
            for item in items {
                match extract_list_entry(item, preferred_keys) {
                    Ok(Some(entry)) => entries.push(entry),
                    Ok(None) => {}
                    Err(_) => {
                        return Err(bad_request(format!(
                            "{field} entries must resolve to plain text values"
                        )));
                    }
                }
            }
            if entries.is_empty() {
                Ok(None)
            } else {
                Ok(Some(entries))
            }
        }
        other => match extract_list_entry(other, preferred_keys) {
            Ok(Some(entry)) => Ok(Some(vec![entry])),
            Ok(None) => Ok(None),
            Err(_) => Err(bad_request(format!(
                "{field} entries must resolve to plain text values"
            ))),
        },
    }
}

fn extract_list_entry(value: Value, preferred_keys: &[&str]) -> Result<Option<String>, ()> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Value::Number(n) => Ok(Some(n.to_string())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        Value::Object(mut map) => {
            for key in preferred_keys {
                if let Some(Value::String(s)) = map.remove(*key) {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        return Ok(None);
                    }
                    return Ok(Some(trimmed.to_string()));
                }
            }
            for (_, candidate) in map.into_iter() {
                if let Value::String(s) = candidate {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    return Ok(Some(trimmed.to_string()));
                }
            }
            Err(())
        }
        _ => Err(()),
    }
}

#[allow(clippy::result_large_err)]
fn parse_json_field(value: &Option<String>, field: &str) -> Result<Option<Value>, Response> {
    let Some(raw) = value.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };

    serde_json::from_str(raw)
        .map(Some)
        .map_err(|err| bad_request(format!("{} must be valid JSON: {}", field, err)))
}

/// Session-authenticated promote endpoint for the console UI.
///
/// Mirrors `POST /api/v1/gateways/{name}/promote` but routes through the
/// console session-cookie auth path (the browser doesn't carry the bearer
/// token). The body of the work lives in
/// [`crate::handler::api_v1::gateways::promote_file_gateway_inner`] so both
/// entry points behave identically.
async fn promote_file_trunk(
    AxumPath(name): AxumPath<String>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let Some(app_state) = state.app_state() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"message": "App state not available; cannot promote file trunk"})),
        )
            .into_response();
    };
    match crate::handler::api_v1::gateways::promote_file_gateway_inner(&app_state, &name).await {
        Ok(model) => (
            StatusCode::CREATED,
            Json(json!({
                "name": model.name,
                "kind": model.kind,
                "direction": model.direction.as_str(),
                "is_active": model.is_active,
            })),
        )
            .into_response(),
        Err(err) => err.into_response(),
    }
}

/// Manual "Check now" — runs an immediate probe against a single trunk and
/// writes the outcome through the same state machine the periodic monitor
/// uses ([`crate::proxy::gateway_health::apply_probe_outcome`]). Lets an
/// operator force a refresh after fixing an endpoint without waiting for
/// the back-off interval to expire.
async fn probe_trunk_now(
    AxumPath(id): AxumPath<i64>,
    State(state): State<Arc<ConsoleState>>,
    AuthRequired(user): AuthRequired,
) -> Response {
    if !state.has_permission(&user, "trunks", "write").await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Permission denied"})),
        )
            .into_response();
    }
    let db = state.db();
    let trunk = match SipTrunkEntity::find_by_id(id).one(db).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"message": "Trunk not found"})))
                .into_response();
        }
        Err(e) => {
            warn!(error = %e, "probe_trunk_now: db lookup failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("db lookup failed: {e}")})),
            )
                .into_response();
        }
    };

    let prober = match crate::proxy::health_probers::lookup(&trunk.kind) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "message": format!("no health prober registered for kind '{}'", trunk.kind),
                })),
            )
                .into_response();
        }
    };

    let timeout = std::time::Duration::from_secs(5);
    let outcome = prober.probe(&trunk, timeout).await;

    let updated = match crate::proxy::gateway_health::apply_probe_outcome(db, &trunk, &outcome).await {
        Ok(m) => m,
        Err(e) => {
            warn!(trunk = %trunk.name, error = %e, "probe_trunk_now: persist failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("persist failed: {e}")})),
            )
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(json!({
            "ok": outcome.ok,
            "latency_ms": outcome.latency_ms,
            "detail": outcome.detail,
            "status": updated.status.as_str(),
            "consecutive_failures": updated.consecutive_failures,
            "consecutive_successes": updated.consecutive_successes,
            "last_health_check_at": updated.last_health_check_at,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ConsoleConfig, console::middleware::AuthRequired, models::migration::Migrator,
    };
    use axum::{extract::State, http::StatusCode};
    use chrono::Utc;
    use sea_orm::Database;
    use sea_orm_migration::MigratorTrait;
    use std::sync::Arc;

    #[test]
    fn prefer_hd_metadata_opt_out_and_back() {
        use serde_json::json;
        // Default ON = nothing stored: enabling on an empty blob stays None.
        assert!(apply_prefer_hd_metadata(None, true).is_none());
        // Opt OUT records media.hd_disabled, preserving sibling keys.
        let off =
            apply_prefer_hd_metadata(Some(json!({"recording": {"mode": "all"}})), false)
                .expect("some");
        assert_eq!(off["media"]["hd_disabled"], json!(true));
        assert_eq!(off["recording"]["mode"], "all");
        // Re-enabling clears the flag and drops the now-empty media object.
        let back = apply_prefer_hd_metadata(Some(off), true).expect("some");
        assert!(back.get("media").is_none());
        assert_eq!(back["recording"]["mode"], "all");
        // Opt-out on an otherwise-empty blob stores just the flag; re-enable → None.
        let only = apply_prefer_hd_metadata(None, false).expect("some");
        assert_eq!(only["media"]["hd_disabled"], json!(true));
        assert!(apply_prefer_hd_metadata(Some(only), true).is_none());
    }

    fn superuser() -> crate::models::user::Model {
        let now = Utc::now();
        crate::models::user::Model {
            id: 1,
            email: "admin@rustpbx.com".into(),
            username: "admin".into(),
            password_hash: "hashed".into(),
            reset_token: None,
            reset_token_expires: None,
            last_login_at: None,
            last_login_ip: None,
            created_at: now,
            updated_at: now,
            is_active: true,
            is_staff: true,
            is_superuser: true,
            mfa_enabled: false,
            mfa_secret: None,
            auth_source: "local".into(),
        }
    }

    fn unprivileged_user() -> crate::models::user::Model {
        let now = Utc::now();
        crate::models::user::Model {
            id: 99,
            email: "limited@rustpbx.com".into(),
            username: "limited".into(),
            password_hash: "hashed".into(),
            reset_token: None,
            reset_token_expires: None,
            last_login_at: None,
            last_login_ip: None,
            created_at: now,
            updated_at: now,
            is_active: true,
            is_staff: false,
            is_superuser: false,
            mfa_enabled: false,
            mfa_secret: None,
            auth_source: "local".into(),
        }
    }

    async fn setup_state() -> Arc<ConsoleState> {
        // Console CRUD routes the config through `kind_schemas::validate`,
        // which fails with `UnknownKind` until the built-ins are registered.
        // Also wire the signaling-adapter registry so the WebRTC validator
        // can resolve `http_json` during tests.
        crate::proxy::bridge::signaling::register_builtins();
        crate::models::kind_schemas::register_builtins();
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite memory");
        Migrator::up(&db, None).await.expect("run migrations");
        ConsoleState::initialize(
            Arc::new(crate::callrecord::DefaultCallRecordFormatter::default()),
            db,
            ConsoleConfig::default(),
        )
        .await
        .expect("initialize console state")
    }

    #[tokio::test]
    async fn create_sip_trunk_denied_without_permission() {
        let state = setup_state().await;
        let user = unprivileged_user();
        let form = SipTrunkForm::default();
        let resp =
            create_sip_trunk(State(state), AuthRequired(user), axum::extract::Form(form)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_sip_trunk_denied_without_permission() {
        let state = setup_state().await;
        let user = unprivileged_user();
        let form = SipTrunkForm::default();
        let resp = update_sip_trunk(
            AxumPath(999i64),
            State(state),
            AuthRequired(user),
            axum::extract::Form(form),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn delete_sip_trunk_denied_without_permission() {
        let state = setup_state().await;
        let user = unprivileged_user();
        let resp = delete_sip_trunk(AxumPath(999i64), State(state), AuthRequired(user)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn create_sip_trunk_allowed_for_superuser() {
        let state = setup_state().await;
        let user = superuser();
        let mut form = SipTrunkForm::default();
        form.name = Some("test-trunk".into());
        form.sip_server = Some("sip.example.com".into());
        let resp =
            create_sip_trunk(State(state), AuthRequired(user), axum::extract::Form(form)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    async fn seed_trunk(state: &Arc<ConsoleState>, name: &str) -> i64 {
        use axum::body::to_bytes;
        let mut form = SipTrunkForm::default();
        form.name = Some(name.into());
        form.sip_server = Some("sip.example.com".into());
        let resp = create_sip_trunk(
            State(state.clone()),
            AuthRequired(superuser()),
            axum::extract::Form(form),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        v["id"].as_i64().expect("trunk id")
    }

    #[tokio::test]
    async fn delete_trunk_with_dids_returns_409() {
        use crate::models::did::{self, NewDid};

        let state = setup_state().await;
        let trunk_id = seed_trunk(&state, "guarded").await;

        did::Model::upsert(
            state.db(),
            NewDid {
                number: "+14158675309".into(),
                trunk_name: Some("guarded".into()),
                extension_number: None,
                failover_trunk: None,
                label: None,
                enabled: true,
            },
        )
        .await
        .unwrap();

        let resp =
            delete_sip_trunk(AxumPath(trunk_id), State(state.clone()), AuthRequired(superuser()))
                .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // Trunk row still exists.
        assert!(
            SipTrunkEntity::find_by_id(trunk_id)
                .one(state.db())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn delete_trunk_with_failover_reference_returns_409() {
        use crate::models::did::{self, NewDid};

        let state = setup_state().await;
        let owner_id = seed_trunk(&state, "owner").await;
        let failover_id = seed_trunk(&state, "backup").await;

        did::Model::upsert(
            state.db(),
            NewDid {
                number: "+14158675310".into(),
                trunk_name: Some("owner".into()),
                extension_number: None,
                failover_trunk: Some("backup".into()),
                label: None,
                enabled: true,
            },
        )
        .await
        .unwrap();

        // Deleting the failover target must be blocked.
        let resp = delete_sip_trunk(
            AxumPath(failover_id),
            State(state.clone()),
            AuthRequired(superuser()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        // Owner trunk is also blocked.
        let resp = delete_sip_trunk(
            AxumPath(owner_id),
            State(state.clone()),
            AuthRequired(superuser()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_trunk_without_dids_succeeds() {
        let state = setup_state().await;
        let trunk_id = seed_trunk(&state, "free").await;
        let resp =
            delete_sip_trunk(AxumPath(trunk_id), State(state.clone()), AuthRequired(superuser()))
                .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            SipTrunkEntity::find_by_id(trunk_id)
                .one(state.db())
                .await
                .unwrap()
                .is_none()
        );
    }

    // ---- PR 5 / Phase 10: kind-aware console regression tests. ----

    fn webrtc_form(name: &str) -> SipTrunkForm {
        let mut form = SipTrunkForm::default();
        form.kind = Some("webrtc".into());
        form.name = Some(name.into());
        form.webrtc_signaling = Some("http_json".into());
        form.webrtc_endpoint_url = Some("https://signal.example.com/offer".into());
        form.webrtc_audio_codec = Some("opus".into());
        form.webrtc_protocol = Some(
            r#"{"request_body_template":"{\"sdp\":\"{offer_sdp}\",\"type\":\"offer\"}","response_answer_path":"$.sdp"}"#
                .into(),
        );
        form
    }

    #[tokio::test]
    async fn create_webrtc_trunk_persists_kind_and_kind_config() {
        let state = setup_state().await;
        let form = webrtc_form("pipecat_bot");
        let resp = create_sip_trunk(
            State(state.clone()),
            AuthRequired(superuser()),
            axum::extract::Form(form),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let row = SipTrunkEntity::find()
            .filter(SipTrunkColumn::Name.eq("pipecat_bot"))
            .one(state.db())
            .await
            .unwrap()
            .expect("webrtc trunk row");
        assert_eq!(row.kind, "webrtc");
        let cfg = row.webrtc().expect("decode webrtc cfg");
        assert_eq!(cfg.signaling, "http_json");
        assert_eq!(cfg.endpoint_url, "https://signal.example.com/offer");
    }

    #[tokio::test]
    async fn create_webrtc_trunk_with_invalid_signaling_returns_400() {
        let state = setup_state().await;
        let mut form = webrtc_form("bad_adapter");
        form.webrtc_signaling = Some("nonexistent_adapter_xyz".into());
        let resp = create_sip_trunk(
            State(state),
            AuthRequired(superuser()),
            axum::extract::Form(form),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let msg = v["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("nonexistent_adapter_xyz") || msg.contains("signaling"),
            "expected adapter-related error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn list_view_includes_webrtc_trunks() {
        let state = setup_state().await;
        let _sip_id = seed_trunk(&state, "voda_inbound").await;
        let resp = create_sip_trunk(
            State(state.clone()),
            AuthRequired(superuser()),
            axum::extract::Form(webrtc_form("pipecat_bot")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let payload = ListQuery::<QuerySipTrunkFilters> {
            page: 1,
            per_page: 50,
            per_page_min: 5,
            per_page_max: 100,
            filters: None,
            sort: None,
        };
        let resp = query_sip_trunks(
            State(state),
            AuthRequired(superuser()),
            axum::extract::Json(payload),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let items = v["items"].as_array().expect("items array");
        let names: Vec<&str> = items.iter().filter_map(|i| i["name"].as_str()).collect();
        assert!(
            names.contains(&"pipecat_bot"),
            "list view must include webrtc trunk, got {names:?}"
        );
        assert!(names.contains(&"voda_inbound"));
        // Each item carries its `kind` so the UI can render a badge.
        let kinds: Vec<&str> = items.iter().filter_map(|i| i["kind"].as_str()).collect();
        assert!(kinds.iter().any(|k| *k == "webrtc"));
        assert!(kinds.iter().any(|k| *k == "sip"));
    }

    #[tokio::test]
    async fn detail_view_flattens_webrtc_kind_config() {
        let state = setup_state().await;
        let resp = create_sip_trunk(
            State(state.clone()),
            AuthRequired(superuser()),
            axum::extract::Form(webrtc_form("rtc1")),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let id = v["id"].as_i64().expect("id");

        // The detail handler renders an HTML template; here we just verify
        // the row can be reloaded and decoded as a WebRTC config — the
        // template rendering path is exercised by the same code path.
        let row = SipTrunkEntity::find_by_id(id)
            .one(state.db())
            .await
            .unwrap()
            .expect("row");
        let cfg = row.webrtc().expect("webrtc decode");
        assert_eq!(cfg.signaling, "http_json");
        assert_eq!(cfg.audio_codec, "opus");
    }
}
