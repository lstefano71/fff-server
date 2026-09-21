//! A REST server over the `fff-search` engine, returning typed objects rather than the prose
//! the MCP server produces. See DESIGN.md for the reasoning behind every choice here.

pub mod config;
pub mod dto;
pub mod error;
pub mod extract;
pub mod guard;
pub mod logging;
pub mod paths;
pub mod query;
pub mod routes;
pub mod state;
pub mod workspace;

use std::time::Duration;

use axum::Router;
use axum::routing::get;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::config::Config;
use crate::state::AppState;

/// The `fff-search` release this binary is built against. Reported by `/v1/health` so a
/// client can tell which engine produced a given result shape.
pub const ENGINE_VERSION: &str = "0.11.0";

/// How often the maintenance loop looks for due rescans and idle workspaces. The intervals
/// themselves are per-workspace and cost-derived; this is only the polling granularity.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

#[derive(OpenApi)]
#[openapi(
    info(
        title = "fff-server",
        description = "Typed HTTP access to the fff file-search engine.",
        license(name = "Unlicense", identifier = "Unlicense"),
    ),
    // An absolute default, because code generators reject a relative server url and then
    // demand the base address be wired up by hand. The bind address is configurable, so a
    // client pointed elsewhere simply overrides this.
    servers((url = "http://localhost:8080", description = "Default local bind")),
    components(schemas(crate::error::Problem)),
    tags(
        (name = "meta", description = "Server health and contract"),
        (name = "workspaces", description = "Indexed roots and their lifecycle"),
        (name = "search", description = "Fuzzy path search and glob matching"),
        (name = "grep", description = "Content search"),
        (name = "lifecycle", description = "Rescan, git refresh, and ranking feedback"),
    ),
)]
pub struct ApiDoc;

/// Builds the router and the OpenAPI document from one source, so they cannot disagree.
pub fn build(config: Config) -> (Router, utoipa::openapi::OpenApi, AppState) {
    let state = AppState::new(config);

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(routes::meta::health))
        .routes(routes!(
            routes::workspaces::create,
            routes::workspaces::list
        ))
        .routes(routes!(routes::workspaces::get, routes::workspaces::delete))
        .routes(routes!(routes::search::search))
        .routes(routes!(routes::search::search_directories))
        .routes(routes!(routes::search::search_mixed))
        .routes(routes!(routes::search::glob))
        .routes(routes!(routes::grep::grep))
        .routes(routes!(routes::grep::multi_grep))
        .routes(routes!(routes::lifecycle::rescan))
        .routes(routes!(routes::lifecycle::git_refresh))
        .routes(routes!(routes::lifecycle::track_access))
        .routes(routes!(routes::lifecycle::track_query))
        .routes(routes!(routes::lifecycle::history))
        .routes(routes!(routes::lifecycle::parse_query))
        .with_state(state.clone())
        .split_for_parts();

    // Served from the same document the snapshot test asserts on.
    let spec = api.clone();
    let router = router.route(
        "/openapi.json",
        get(move || {
            let spec = spec.clone();
            async move { axum::Json(spec) }
        }),
    );

    (router, api, state)
}

/// Periodic rescans and idle eviction. Runs until the process exits.
pub async fn maintenance(state: AppState) {
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if state.pool.is_empty() {
            continue;
        }
        let pool = state.pool.clone();
        // Rescans and teardown both block; keep them off the async workers.
        match tokio::task::spawn_blocking(move || pool.sweep()).await {
            Ok((0, 0)) => {}
            Ok((rescanned, evicted)) => {
                tracing::info!(rescanned, evicted, "maintenance sweep");
            }
            Err(e) => tracing::error!(error = %e, "maintenance sweep failed"),
        }
    }
}

/// The contract, for the snapshot test and for `openapi.json`.
pub fn openapi_document() -> utoipa::openapi::OpenApi {
    build(Config::default()).1
}
