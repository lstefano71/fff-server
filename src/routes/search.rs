use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use fff_search::{FuzzySearchOptions, PaginationArgs};

use crate::dto::file::{DirItemDto, FileItemDto, ScoreDto, utf16_ranges};
use crate::dto::search::{
    DirHit, DirSearchResponse, GlobRequest, MixedHit, MixedSearchResponse, SearchHit,
    SearchRequest, SearchResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extract::Json as ProblemJson;
use crate::query::{PreparedQuery, Preset};
use crate::state::AppState;
use crate::workspace::Workspace;

/// Resolved paging plus the engine options, so the four handlers agree.
struct Params {
    page: usize,
    page_size: usize,
    warnings: Vec<String>,
}

fn prepare(
    state: &AppState,
    req: &SearchRequest,
    preset: Preset,
) -> ApiResult<(PreparedQuery, Params)> {
    if req.query.is_some() && req.structured.is_some() {
        return Err(ApiError::InvalidBody(
            "supply either `query` or `structured`, not both".into(),
        ));
    }

    let prepared = match (&req.query, &req.structured) {
        (Some(raw), _) => PreparedQuery::raw(raw, preset),
        (None, Some(s)) => PreparedQuery::structured(s, preset),
        // An empty query is legitimate: it ranks the whole index by frecency.
        (None, None) => PreparedQuery::raw("", preset),
    };

    let warnings = req
        .query
        .as_deref()
        .map(crate::query::warnings_for)
        .unwrap_or_default();

    Ok((
        prepared,
        Params {
            page: req.page.unwrap_or(0),
            page_size: req
                .page_size
                .unwrap_or(state.config.defaults.page_size)
                .max(1),
            warnings,
        },
    ))
}

fn options<'a>(
    req: &'a SearchRequest,
    params: &Params,
    base: &'a std::path::Path,
) -> FuzzySearchOptions<'a> {
    FuzzySearchOptions {
        max_threads: req.max_threads.unwrap_or(0),
        current_file: req.current_file.as_deref(),
        project_path: Some(base),
        combo_boost_score_multiplier: req.combo_boost_multiplier.unwrap_or(100),
        min_combo_count: req.min_combo_count.unwrap_or(3),
        pagination: PaginationArgs {
            offset: params.page.saturating_mul(params.page_size),
            limit: params.page_size,
        },
    }
}

fn workspace(state: &AppState, id: &str) -> ApiResult<Arc<Workspace>> {
    state
        .pool
        .get(id)
        .ok_or_else(|| ApiError::NotFound(format!("no workspace with id {id:?}")))
}

/// Searches run on a blocking thread: they are synchronous CPU-bound Rust that fans out
/// across the engine's own rayon pool, and must never occupy a tokio worker.
///
/// The closure also holds the picker read guard while converting results. That is required,
/// not incidental: `SearchResult` borrows `&FileItem` out of the index, so every field has
/// to be copied into owned DTOs before the guard drops.
async fn blocking<T, F>(work: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ApiResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ApiError::Internal(format!("search task failed: {e}")))?
}

/// Fuzzy file search.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/search",
    tag = "search",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = SearchRequest,
    responses(
        (status = 200, description = "Ranked file matches", body = SearchResponse),
        (status = 400, description = "Malformed request", body = crate::error::Problem),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn search(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<SearchRequest>,
) -> ApiResult<Json<SearchResponse>> {
    let ws = workspace(&state, &id)?;
    let (prepared, params) = prepare(&state, &req, Preset::FileSearch)?;

    blocking(move || {
        let guard = ws.picker.read()?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ApiError::NotReady("index is still building".into()))?;
        let base = picker.base_path().to_owned();
        let display = ws.root.display.clone();

        let qt = ws.query_tracker.read()?;
        let query = prepared.query();
        let result = picker.fuzzy_search(&query, qt.as_ref(), options(&req, &params, &base));

        let items = result
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let dto = FileItemDto::build(item, picker, &display);
                let ranges = result
                    .match_byte_offsets
                    .get(i)
                    .map(|r| utf16_ranges(&dto.relative_path, r))
                    .unwrap_or_default();
                SearchHit {
                    score: ScoreDto::from(&result.scores[i]),
                    match_ranges_utf16: ranges,
                    item: dto,
                }
            })
            .collect::<Vec<_>>();

        let has_more = params
            .page
            .saturating_add(1)
            .saturating_mul(params.page_size)
            < result.total_matched;

        Ok(Json(SearchResponse {
            items,
            total_matched: result.total_matched,
            total_files: result.total_files,
            location: result.location.map(Into::into),
            page: params.page,
            page_size: params.page_size,
            has_more,
            warnings: params.warnings.clone(),
        }))
    })
    .await
}

/// Fuzzy directory search.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/search/directories",
    tag = "search",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = SearchRequest,
    responses(
        (status = 200, description = "Ranked directory matches", body = DirSearchResponse),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn search_directories(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<SearchRequest>,
) -> ApiResult<Json<DirSearchResponse>> {
    let ws = workspace(&state, &id)?;
    let (prepared, params) = prepare(&state, &req, Preset::DirSearch)?;

    blocking(move || {
        let guard = ws.picker.read()?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ApiError::NotReady("index is still building".into()))?;
        let base = picker.base_path().to_owned();
        let display = ws.root.display.clone();

        let query = prepared.query();
        let result = picker.fuzzy_search_directories(&query, options(&req, &params, &base));

        let items = result
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| DirHit {
                item: DirItemDto::build(item, picker, &display),
                score: ScoreDto::from(&result.scores[i]),
            })
            .collect::<Vec<_>>();

        let has_more = params
            .page
            .saturating_add(1)
            .saturating_mul(params.page_size)
            < result.total_matched;

        Ok(Json(DirSearchResponse {
            items,
            total_matched: result.total_matched,
            total_dirs: result.total_dirs,
            page: params.page,
            page_size: params.page_size,
            has_more,
            warnings: params.warnings.clone(),
        }))
    })
    .await
}

/// Fuzzy search over files and directories together.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/search/mixed",
    tag = "search",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = SearchRequest,
    responses(
        (status = 200, description = "Ranked file and directory matches", body = MixedSearchResponse),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn search_mixed(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<SearchRequest>,
) -> ApiResult<Json<MixedSearchResponse>> {
    let ws = workspace(&state, &id)?;
    let (prepared, params) = prepare(&state, &req, Preset::MixedSearch)?;

    blocking(move || {
        let guard = ws.picker.read()?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ApiError::NotReady("index is still building".into()))?;
        let base = picker.base_path().to_owned();
        let display = ws.root.display.clone();

        let qt = ws.query_tracker.read()?;
        let query = prepared.query();
        let result = picker.fuzzy_search_mixed(&query, qt.as_ref(), options(&req, &params, &base));

        let items = result
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let score = ScoreDto::from(&result.scores[i]);
                match item {
                    fff_search::types::MixedItemRef::File(f) => MixedHit::File {
                        item: FileItemDto::build(f, picker, &display),
                        score,
                    },
                    fff_search::types::MixedItemRef::Dir(d) => MixedHit::Directory {
                        item: DirItemDto::build(d, picker, &display),
                        score,
                    },
                }
            })
            .collect::<Vec<_>>();

        let has_more = params
            .page
            .saturating_add(1)
            .saturating_mul(params.page_size)
            < result.total_matched;

        Ok(Json(MixedSearchResponse {
            items,
            total_matched: result.total_matched,
            total_files: result.total_files,
            total_dirs: result.total_dirs,
            location: result.location.map(Into::into),
            page: params.page,
            page_size: params.page_size,
            has_more,
            warnings: params.warnings.clone(),
        }))
    })
    .await
}

/// Literal glob match, frecency-ranked, bypassing the query parser.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/glob",
    tag = "search",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = GlobRequest,
    responses(
        (status = 200, description = "Matching files", body = SearchResponse),
        (status = 400, description = "Invalid glob pattern", body = crate::error::Problem),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn glob(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<GlobRequest>,
) -> ApiResult<Json<SearchResponse>> {
    let ws = workspace(&state, &id)?;
    let page = req.page.unwrap_or(0);
    let page_size = req
        .page_size
        .unwrap_or(state.config.defaults.page_size)
        .max(1);

    // Folded here rather than rejected: a literal glob field is unambiguously a path
    // pattern, so a backslash can only have been meant as a separator.
    let pattern = req.pattern.replace('\\', "/");
    if pattern.trim().is_empty() {
        return Err(ApiError::InvalidBody("pattern must not be empty".into()));
    }

    blocking(move || {
        let guard = ws.picker.read()?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ApiError::NotReady("index is still building".into()))?;
        let base = picker.base_path().to_owned();
        let display = ws.root.display.clone();

        let result = picker.glob(
            &pattern,
            FuzzySearchOptions {
                max_threads: req.max_threads.unwrap_or(0),
                current_file: req.current_file.as_deref(),
                project_path: Some(&base),
                combo_boost_score_multiplier: 100,
                min_combo_count: 3,
                pagination: PaginationArgs {
                    offset: page.saturating_mul(page_size),
                    limit: page_size,
                },
            },
        );

        let items = result
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| SearchHit {
                item: FileItemDto::build(item, picker, &display),
                score: ScoreDto::from(&result.scores[i]),
                // A literal glob has no fuzzy match ranges to report.
                match_ranges_utf16: Vec::new(),
            })
            .collect::<Vec<_>>();

        let has_more = page.saturating_add(1).saturating_mul(page_size) < result.total_matched;

        Ok(Json(SearchResponse {
            items,
            total_matched: result.total_matched,
            total_files: result.total_files,
            location: None,
            page,
            page_size,
            has_more,
            warnings: Vec::new(),
        }))
    })
    .await
}
