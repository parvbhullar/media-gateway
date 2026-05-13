// TODO(wave-2-followup / Phase 10): kind-aware Trunks UI. This module still
// treats every trunk row as SIP — non-SIP kinds are filtered out at read time
// and unreachable from this handler set. The full kind-aware form (signaling
// adapter dropdown, ICE servers, etc.) lands in Phase 10.
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
    models::sip_trunk::{
        ActiveModel as SipTrunkActiveModel, Column as SipTrunkColumn, Entity as SipTrunkEntity,
        SipTransport, SipTrunkConfig, SipTrunkDirection, SipTrunkStatus,
    },
    proxy::routing::ConfigOrigin,
};
use axum::{
    Json, Router,
    extract::{Form, Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
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
                .any(|t| matches!(t.origin, ConfigOrigin::File(_)))
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
        Ok(Some(model)) if model.kind == "sip" => {
            #[allow(unused_mut)]
            let mut model_json = serde_json::to_value(&model).unwrap_or(json!({}));
            // Flatten the SIP-typed view of `kind_config` into the top level
            // of the JSON the template sees, preserving the legacy field names
            // the form relies on.
            if let Ok(sip_cfg) = model.sip()
                && let (Some(obj), Ok(Value::Object(flat))) =
                    (model_json.as_object_mut(), serde_json::to_value(&sip_cfg))
            {
                for (k, v) in flat {
                    obj.insert(k, v);
                }
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
        Ok(Some(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "SIP trunk not found"})),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": "SIP trunk not found"})),
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

    if let Err(response) = apply_form_to_active_model(&mut active, &form, now, false, None) {
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
        Ok(Some(model)) if model.kind == "sip" => model,
        Ok(Some(_)) | Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "SIP trunk not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load sip trunk {} for update: {}", id, err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": format!("Failed to update SIP trunk: {}", err)})),
            )
                .into_response();
        }
    };

    let existing_sip_cfg = model.sip().unwrap_or_default();
    let mut active: SipTrunkActiveModel = model.into();
    let now = Utc::now();
    if let Err(response) =
        apply_form_to_active_model(&mut active, &form, now, true, Some(existing_sip_cfg))
    {
        return response;
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
        Ok(Some(model)) if model.kind == "sip" => model,
        Ok(Some(_)) | Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message": "SIP trunk not found"})),
            )
                .into_response();
        }
        Err(err) => {
            warn!("failed to load sip trunk {} for delete: {}", id, err);
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

    // SIP-only view: hide non-SIP rows. Phase 10 will replace this with a
    // kind-aware Trunks page.
    let mut selector = SipTrunkEntity::find().filter(SipTrunkColumn::Kind.eq("sip"));

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
        .map(|model| serde_json::to_value(&model).unwrap_or_else(|_| json!({})))
        .collect();

    // Issue #179: collect file-sourced trunks from in-memory snapshot
    let file_trunks: Vec<Value> = if let Some(app_state) = state.app_state() {
        let snapshot = app_state.sip_server().inner.data_context.trunks_snapshot();
        let mut file_items: Vec<Value> = snapshot
            .into_iter()
            .filter_map(|(name, trunk)| {
                if let ConfigOrigin::File(ref path) = trunk.origin {
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

#[allow(clippy::result_large_err)]
fn apply_form_to_active_model(
    active: &mut SipTrunkActiveModel,
    form: &SipTrunkForm,
    now: DateTime<Utc>,
    is_update: bool,
    existing_sip_cfg: Option<SipTrunkConfig>,
) -> Result<(), Response> {
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
        active.kind = Set("sip".to_string());
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
    if !is_update || form.metadata.is_some() {
        active.metadata = Set(metadata);
    }

    // Build the SIP-typed `kind_config` blob. On update we start from the
    // previously-decoded config so omitted fields are preserved; on create we
    // start from defaults.
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

    if let Err(err) = sip_cfg.validate() {
        return Err(bad_request(format!("invalid SIP trunk config: {err}")));
    }
    let kind_config_json = serde_json::to_value(&sip_cfg)
        .map_err(|e| bad_request(format!("failed to serialize SIP config: {e}")))?;
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
}
