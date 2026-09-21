use axum::extract::State;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use crate::state::AppState;

/// Server liveness and, once the pool exists, per-workspace readiness.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// Always `"ok"` when the process is answering.
    #[schema(example = "ok")]
    pub status: &'static str,
    /// This server's version.
    pub version: &'static str,
    /// The `fff-search` release this binary is built against.
    #[schema(example = "0.11.0")]
    pub engine_version: &'static str,
    /// Seconds since process start.
    pub uptime_seconds: u64,
    /// Number of live workspaces in the pool.
    pub workspace_count: usize,
}

#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "meta",
    responses((status = 200, description = "Server is answering", body = Health)),
)]
pub async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        engine_version: crate::ENGINE_VERSION,
        uptime_seconds: state.started.elapsed().as_secs(),
        workspace_count: 0,
    })
}
