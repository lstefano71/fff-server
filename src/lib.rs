//! A REST server over the `fff-search` engine, returning typed objects rather than the prose
//! the MCP server produces. See DESIGN.md for the reasoning behind every choice here.

pub mod config;
pub mod error;
pub mod logging;
pub mod routes;
pub mod state;

use axum::routing::get;
use axum::Router;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::config::Config;
use crate::state::AppState;

/// The `fff-search` release this binary is built against. Reported by `/v1/health` so a
/// client can tell which engine produced a given result shape.
pub const ENGINE_VERSION: &str = "0.11.0";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "fff-server",
        description = "Typed HTTP access to the fff file-search engine.",
        license(name = "MIT"),
    ),
    components(schemas(crate::error::Problem)),
    tags(
        (name = "meta", description = "Server health and contract"),
    ),
)]
pub struct ApiDoc;

/// Builds the router and the OpenAPI document from one source, so they cannot disagree.
pub fn build(config: Config) -> (Router, utoipa::openapi::OpenApi) {
    let state = AppState::new(config);

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(routes::meta::health))
        .with_state(state)
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

    (router, api)
}

/// The contract, for the snapshot test and for `openapi.json`.
pub fn openapi_document() -> utoipa::openapi::OpenApi {
    build(Config::default()).1
}
