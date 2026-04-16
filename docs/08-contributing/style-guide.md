# Style Guide

## Rust Conventions

- Format: `cargo fmt` (standard rustfmt)
- Lint: `cargo clippy`
- Edition: 2024

## Naming

- SeaORM entities: `rustpbx_` table prefix
- View types: Never serialize `Model` directly — use view structs with `#[derive(Serialize)]` and `From<Model>` (e.g., `GatewayView` in `src/handler/api_v1/gateways.rs`)
- API router files: One file per resource group in `src/handler/api_v1/`
- Test files: `tests/api_v1_<group>.rs` for integration tests, `src/<module>/tests/` or inline `#[cfg(test)]` for unit tests

## Error Handling

Use `ApiError` constructors from `src/handler/api_v1/error.rs`:

| Constructor | Status | Usage |
|-------------|--------|-------|
| `ApiError::bad_request(msg)` | 400 | Invalid input, validation failures |
| `ApiError::not_found(msg)` | 404 | Resource does not exist |
| `ApiError::conflict(msg)` | 409 | Duplicate create, referenced resource cannot be deleted |
| `ApiError::not_implemented(msg)` | 501 | Stub endpoint not yet built |
| `ApiError::internal(msg)` | 500 | Unexpected server errors |

Return type for handlers: `ApiResult<T>` (alias for `Result<T, ApiError>`).

## Adapter Pattern (SHELL-05)

API handlers call pure data functions, not framework-coupled code:

- **Data functions**: `pub(crate)` visibility, accept `&DatabaseConnection` + typed input, return `Result<T, ApiError>`
- **Non-DB helpers**: Accept `&AppState` (for registrar, locator, reload guard)
- **Forbidden in data fn signatures**: `State<>`, `Response`, `AuthRequired`, or any Axum extractor
- Both HTML console and JSON API handlers call the same data functions

Example from gateways:

```rust
// Pure data function (adapter)
async fn trunk_by_name(
    db: &sea_orm::DatabaseConnection,
    name: &str,
) -> ApiResult<Option<TrunkModel>> { ... }

// HTTP handler calls the adapter
async fn get_gateway(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<GatewayView>> {
    let row = trunk_by_name(state.db(), &name).await?
        .ok_or_else(|| ApiError::not_found(...))?;
    Ok(Json(row.into()))
}
```

## Pagination

List endpoints return `PaginatedResponse<T>`:

```json
{"items": [...], "page": 1, "page_size": 20, "total": 42}
```

Default: page 1, page_size 20, max 200. Defined in `src/handler/api_v1/common.rs`.

## Commit Messages

Conventional commit style:

- `feat(scope): description` — new feature
- `fix(scope): description` — bug fix
- `docs(scope): description` — documentation
- `test(scope): description` — tests
- `refactor(scope): description` — refactoring

Scope is typically the module or phase number (e.g., `feat(02-01): add trunk group CRUD`).

## Feature Flags

New optional subsystems should be gated behind a Cargo feature in `Cargo.toml`. Use `#[cfg(feature = "...")]` in code and document the feature in [Dev Setup](dev-setup.md).

## Dependencies

- HTTP framework: Axum 0.8
- ORM: SeaORM 1.x with SeaORM Migration
- SIP stack: rsipstack
- Media: audio-codec, rustrtc
- Async runtime: Tokio
- Serialization: serde + serde_json
- Config: toml / toml_edit
