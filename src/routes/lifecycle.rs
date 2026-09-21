use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use fff_query_parser::{
    AiGrepConfig, DirSearchConfig, FileSearchConfig, GrepConfig, MixedSearchConfig, QueryParser,
};

use crate::dto::lifecycle::{
    GitRefreshResponse, HistoryQuery, HistoryResponse, ParsePreset, ParseQueryRequest,
    ParseQueryResponse, TrackAccessRequest, TrackAccessResponse, TrackQueryRequest,
};
use crate::dto::workspace::WorkspaceResource;
use crate::error::{ApiError, ApiResult};
use crate::extract::Json as ProblemJson;
use crate::state::AppState;
use crate::workspace::Workspace;

fn workspace(state: &AppState, id: &str) -> ApiResult<Arc<Workspace>> {
    state
        .pool
        .get(id)
        .ok_or_else(|| ApiError::NotFound(format!("no workspace with id {id:?}")))
}

fn resource(state: &AppState, ws: &Workspace) -> WorkspaceResource {
    WorkspaceResource::build(
        ws,
        state.pool.rescan_interval(ws).as_secs(),
        state.pool.idle_timeout(ws).as_secs(),
    )
}

/// Resolves a client-supplied path to an absolute path inside the workspace.
///
/// Accepts repo-relative or absolute, either separator. Rejects anything that escapes the
/// root: a frecency entry for a file outside the workspace would be recorded against a
/// database this workspace owns, which is both wrong and a way to probe the filesystem.
fn resolve_in_workspace(ws: &Workspace, input: &str) -> ApiResult<(PathBuf, String)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidBody("path must not be empty".into()));
    }

    let candidate = std::path::Path::new(trimmed);
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        ws.root.canonical.join(trimmed.replace('\\', "/"))
    };

    // Canonicalise so `..`, casing and short names cannot smuggle a path out of the root.
    let canonical = crate::paths::canonicalize(&absolute)
        .map_err(|e| ApiError::InvalidPath(format!("cannot resolve {trimmed:?}: {e}")))?;

    let relative = canonical
        .strip_prefix(&ws.root.canonical)
        .map_err(|_| {
            ApiError::InvalidPath(format!(
                "{trimmed:?} resolves outside the workspace root {}",
                ws.root.display
            ))
        })?
        .to_string_lossy()
        .replace('\\', "/");

    Ok((canonical, relative))
}

/// Force a rescan now.
///
/// The automatic interval is derived from measured cost and can be up to 30 minutes on a
/// large share, so a client that knows it changed something should say so rather than wait.
/// Returns `202`: the rescan runs in the background, and the returned resource shows
/// `isScanning`.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/rescan",
    tag = "lifecycle",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 202, description = "Rescan triggered", body = WorkspaceResource),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready, or a rescan was throttled", body = crate::error::Problem),
    ),
)]
pub async fn rescan(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<WorkspaceResource>)> {
    let ws = workspace(&state, &id)?;
    let target = ws.clone();
    tokio::task::spawn_blocking(move || target.rescan())
        .await
        .map_err(|e| ApiError::Internal(format!("rescan task failed: {e}")))??;
    Ok((StatusCode::ACCEPTED, Json(resource(&state, &ws))))
}

/// Refresh cached git status.
///
/// Worth calling after a commit or a branch switch: the watcher notices file changes, but
/// status is cached per file and a `git checkout` can change many at once.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/git/refresh",
    tag = "lifecycle",
    params(("id" = String, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Statuses refreshed", body = GitRefreshResponse),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn git_refresh(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<GitRefreshResponse>> {
    let ws = workspace(&state, &id)?;
    // libgit2 work, and it waits on .git/index.lock; keep it off the async runtime.
    let updated = tokio::task::spawn_blocking(move || ws.refresh_git_status())
        .await
        .map_err(|e| ApiError::Internal(format!("git refresh task failed: {e}")))??;
    Ok(Json(GitRefreshResponse { updated }))
}

/// Report that a file was opened.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/track-access",
    tag = "lifecycle",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = TrackAccessRequest,
    responses(
        (status = 200, description = "Recorded", body = TrackAccessResponse),
        (status = 400, description = "Path missing or outside the workspace", body = crate::error::Problem),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
    ),
)]
pub async fn track_access(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<TrackAccessRequest>,
) -> ApiResult<Json<TrackAccessResponse>> {
    let ws = workspace(&state, &id)?;

    tokio::task::spawn_blocking(move || -> ApiResult<Json<TrackAccessResponse>> {
        let (absolute, relative) = resolve_in_workspace(&ws, &req.path)?;

        let guard = ws.frecency.read()?;
        let tracker = guard.as_ref().ok_or_else(|| {
            ApiError::NotReady("frecency database is not available for this workspace".into())
        })?;
        tracker.track_access(&absolute)?;
        let access_count = tracker.access_count(&absolute)?;
        drop(guard);

        // The index caches each file's frecency score, so without this the next search
        // would rank using the pre-access value.
        if let Ok(mut picker_guard) = ws.picker.write()
            && let Some(picker) = picker_guard.as_mut()
            && let Ok(frecency_guard) = ws.frecency.read()
            && let Some(tracker) = frecency_guard.as_ref()
            && let Err(e) = picker.update_single_file_frecency(&absolute, tracker)
        {
            // Not fatal: the database is updated, only the cached score is stale until the
            // next rescan.
            tracing::debug!(error = %e, path = %relative, "could not refresh cached frecency");
        }

        Ok(Json(TrackAccessResponse {
            relative_path: relative,
            access_count,
        }))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("track-access task failed: {e}")))?
}

/// Report which result a query led to.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/track-query",
    tag = "lifecycle",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = TrackQueryRequest,
    responses(
        (status = 204, description = "Recorded"),
        (status = 400, description = "Path outside the workspace, or empty query", body = crate::error::Problem),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
    ),
)]
pub async fn track_query(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<TrackQueryRequest>,
) -> ApiResult<StatusCode> {
    let ws = workspace(&state, &id)?;
    if req.query.trim().is_empty() {
        return Err(ApiError::InvalidBody("query must not be empty".into()));
    }

    tokio::task::spawn_blocking(move || -> ApiResult<StatusCode> {
        let (absolute, _) = resolve_in_workspace(&ws, &req.selected_path)?;

        // track_query_completion takes &mut self, so this needs the write lock.
        let mut guard = ws.query_tracker.write()?;
        let tracker = guard.as_mut().ok_or_else(|| {
            ApiError::NotReady("query history database is not available for this workspace".into())
        })?;
        tracker.track_query_completion(&req.query, &ws.root.canonical, &absolute)?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("track-query task failed: {e}")))?
}

/// A previous query from this workspace's history, newest first.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{id}/history",
    tag = "lifecycle",
    params(
        ("id" = String, Path, description = "Workspace id"),
        ("offset" = Option<usize>, Query, description = "0 is the most recent"),
    ),
    responses(
        (status = 200, description = "Historical query, or null", body = HistoryResponse),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
    ),
)]
pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HistoryQuery>,
) -> ApiResult<Json<HistoryResponse>> {
    let ws = workspace(&state, &id)?;
    let offset = params.offset;

    tokio::task::spawn_blocking(move || -> ApiResult<Json<HistoryResponse>> {
        let guard = ws.query_tracker.read()?;
        let tracker = guard.as_ref().ok_or_else(|| {
            ApiError::NotReady("query history database is not available for this workspace".into())
        })?;
        let query = tracker.get_historical_query(&ws.root.canonical, offset)?;
        Ok(Json(HistoryResponse { query, offset }))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("history task failed: {e}")))?
}

/// Show how the parser decomposes a query, without searching anything.
///
/// Needs no workspace: parsing depends only on the preset.
#[utoipa::path(
    post,
    path = "/v1/parse-query",
    tag = "meta",
    request_body = ParseQueryRequest,
    responses(
        (status = 200, description = "Parsed decomposition", body = ParseQueryResponse),
        (status = 400, description = "Malformed request", body = crate::error::Problem),
    ),
)]
pub async fn parse_query(
    ProblemJson(req): ProblemJson<ParseQueryRequest>,
) -> Json<ParseQueryResponse> {
    let warnings = crate::query::warnings_for(&req.query);
    let parsed = match req.preset {
        ParsePreset::FileSearch => QueryParser::new(FileSearchConfig).parse(&req.query),
        ParsePreset::DirSearch => QueryParser::new(DirSearchConfig).parse(&req.query),
        ParsePreset::MixedSearch => QueryParser::new(MixedSearchConfig).parse(&req.query),
        ParsePreset::Grep => QueryParser::new(GrepConfig).parse(&req.query),
        ParsePreset::AiGrep => QueryParser::new(AiGrepConfig).parse(&req.query),
    };
    Json(ParseQueryResponse::build(&req.query, &parsed, warnings))
}
