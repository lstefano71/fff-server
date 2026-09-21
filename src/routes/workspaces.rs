use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::dto::workspace::{WorkspaceList, WorkspaceResource};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::workspace::CreateWorkspace;
use crate::workspace::pool::Created;

fn resource(state: &AppState, ws: &crate::workspace::Workspace) -> WorkspaceResource {
    WorkspaceResource::build(
        ws,
        state.pool.rescan_interval(ws).as_secs(),
        state.pool.idle_timeout(ws).as_secs(),
    )
}

/// Create or warm a workspace.
///
/// Returns `201` when the index reached the requested readiness stage inside the block
/// budget, and `202` when it did not — the resource is returned either way, and a `202`
/// means poll `GET /v1/workspaces/{id}` rather than that anything failed.
#[utoipa::path(
    post,
    path = "/v1/workspaces",
    tag = "workspaces",
    request_body = CreateWorkspace,
    responses(
        (status = 201, description = "Workspace ready", body = WorkspaceResource),
        (status = 202, description = "Workspace still indexing; poll the resource", body = WorkspaceResource),
        (status = 400, description = "Root missing, not a directory, or too long", body = crate::error::Problem),
        (status = 403, description = "Root refused: filesystem/share root, or outside allowed_roots", body = crate::error::Problem),
        (status = 409, description = "Database already in use for this root", body = crate::error::Problem),
    ),
)]
pub async fn create(
    State(state): State<AppState>,
    crate::extract::Json(req): crate::extract::Json<CreateWorkspace>,
) -> ApiResult<(StatusCode, Json<WorkspaceResource>)> {
    let root = state.pool.resolve_root(&req.root)?;

    // Warm hit: skip the blocking pool entirely.
    if let Some(existing) = state.pool.existing(&root) {
        let body = resource(&state, &existing);
        return Ok((StatusCode::OK, Json(body)));
    }

    // Scanning an 87k-file share took 59.5s. That must never occupy a tokio worker.
    let pool = state.pool.clone();
    let (ws, created) = tokio::task::spawn_blocking(move || pool.create_blocking(root, &req))
        .await
        .map_err(|e| ApiError::Internal(format!("workspace creation task failed: {e}")))??;

    let body = resource(&state, &ws);
    let code = match (created, body.status) {
        (_, crate::dto::workspace::WorkspaceStatus::Ready) => StatusCode::CREATED,
        (Created::Existing, _) => StatusCode::OK,
        // Still building: the client polls rather than holding a request open.
        (Created::New, _) => StatusCode::ACCEPTED,
    };
    Ok((code, Json(body)))
}

/// List live workspaces.
#[utoipa::path(
    get,
    path = "/v1/workspaces",
    tag = "workspaces",
    responses((status = 200, description = "Live workspaces", body = WorkspaceList)),
)]
pub async fn list(State(state): State<AppState>) -> Json<WorkspaceList> {
    let workspaces = state
        .pool
        .list()
        .iter()
        .map(|ws| resource(&state, ws))
        .collect();
    Json(WorkspaceList { workspaces })
}

/// Fetch one workspace, including current readiness. This is the poll target after a `202`.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{id}",
    tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Workspace", body = WorkspaceResource),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
    ),
)]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkspaceResource>> {
    let ws = state
        .pool
        .get(&id)
        .ok_or_else(|| ApiError::NotFound(format!("no workspace with id {id:?}")))?;
    Ok(Json(resource(&state, &ws)))
}

/// Evict a workspace now, releasing its index, watcher threads and database handles.
#[utoipa::path(
    delete,
    path = "/v1/workspaces/{id}",
    tag = "workspaces",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 204, description = "Evicted"),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
    ),
)]
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    // Teardown waits on watcher threads and unmaps LMDB, so keep it off the async runtime.
    let pool = state.pool.clone();
    let removed = tokio::task::spawn_blocking(move || pool.remove(&id))
        .await
        .map_err(|e| ApiError::Internal(format!("eviction task failed: {e}")))?;

    match removed {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(ApiError::NotFound("no such workspace".into())),
    }
}
