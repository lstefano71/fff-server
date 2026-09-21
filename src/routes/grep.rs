use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Json;
use axum::extract::{Path, State};
use fff_query_parser::{AiGrepConfig, GrepConfig, QueryParser};
use fff_search::file_picker::FilePicker;
use fff_search::grep::{GrepResult, GrepSearchOptions};

use crate::dto::file::{FileItemDto, utf16_ranges};
use crate::dto::grep::{
    GrepCursor, GrepMatchDto, GrepPreset, GrepRequest, GrepResponse, MultiGrepRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::extract::Json as ProblemJson;
use crate::state::AppState;
use crate::workspace::Workspace;

/// Sets the engine's abort flag when dropped.
///
/// This is what makes client cancellation real. `spawn_blocking` cannot be cancelled by
/// dropping its `JoinHandle`, so when axum drops a handler future on client disconnect the
/// search would otherwise run to completion, burning CPU for a response nobody will read.
/// Holding this guard across the await means a disconnect trips the flag, and the engine
/// stops at its next check.
struct AbortOnDrop(Arc<AtomicBool>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn workspace(state: &AppState, id: &str) -> ApiResult<Arc<Workspace>> {
    state
        .pool
        .get(id)
        .ok_or_else(|| ApiError::NotFound(format!("no workspace with id {id:?}")))
}

/// Parses constraint tokens with the requested preset.
fn parse_with<'a>(text: &'a str, preset: GrepPreset) -> fff_query_parser::FFFQuery<'a> {
    match preset {
        GrepPreset::Grep => QueryParser::new(GrepConfig).parse(text),
        GrepPreset::AiGrep => QueryParser::new(AiGrepConfig).parse(text),
    }
}

fn parse_constraints(text: &str, preset: GrepPreset) -> fff_query_parser::ConstraintVec<'_> {
    match preset {
        GrepPreset::Grep => QueryParser::new(GrepConfig).parse_constraints(text),
        GrepPreset::AiGrep => QueryParser::new(AiGrepConfig).parse_constraints(text),
    }
}

/// Shared conversion. Holds the picker guard while copying, because `GrepResult` borrows
/// `&FileItem` out of the index.
fn to_response(
    result: GrepResult<'_>,
    picker: &FilePicker,
    display: &str,
    mode: &'static str,
    aborted: bool,
) -> GrepResponse {
    let files: Vec<FileItemDto> = result
        .files
        .iter()
        .map(|f| FileItemDto::build(f, picker, display))
        .collect();

    let matches = result
        .matches
        .iter()
        .map(|m| {
            let ranges = utf16_ranges(&m.line_content, &m.match_byte_offsets);
            GrepMatchDto {
                file_index: m.file_index,
                line_number: m.line_number,
                // The engine reports a byte column; convert it the same way as the ranges,
                // so a C# client can index lineContent with either without translation.
                col_utf16: utf16_ranges(&m.line_content, &[(m.col as u32, m.col as u32)])
                    .first()
                    .map(|r| r.start)
                    .unwrap_or(0),
                byte_offset: m.byte_offset,
                match_ranges_utf16: ranges,
                fuzzy_score: m.fuzzy_score,
                is_definition: m.is_definition,
                context_before: m.context_before.clone(),
                context_after: m.context_after.clone(),
                line_content: m.line_content.clone(),
            }
        })
        .collect::<Vec<_>>();

    GrepResponse {
        total_matched: matches.len(),
        matches,
        files,
        total_files_searched: result.total_files_searched,
        total_files: result.total_files,
        filtered_file_count: result.filtered_file_count,
        files_with_matches: result.files_with_matches,
        // 0 means exhausted, which is the engine's convention.
        next_cursor: (result.next_file_offset > 0)
            .then(|| GrepCursor::from_offset(result.next_file_offset)),
        regex_fallback_error: result.regex_fallback_error.clone(),
        literal_fallback: result.literal_fallback,
        mode,
        aborted,
    }
}

#[allow(clippy::too_many_arguments)]
fn options(
    abort: Arc<AtomicBool>,
    file_offset: usize,
    page_limit: usize,
    casing: fff_search::grep::Casing,
    mode: fff_search::grep::GrepMode,
    max_file_size: Option<u64>,
    max_matches_per_file: Option<usize>,
    before_context: usize,
    after_context: usize,
    classify_definitions: bool,
    trim_whitespace: bool,
    time_budget_ms: u64,
    enforce_time_budget: bool,
) -> GrepSearchOptions {
    let defaults = GrepSearchOptions::default();
    GrepSearchOptions {
        max_file_size: max_file_size.unwrap_or(defaults.max_file_size),
        max_matches_per_file: max_matches_per_file.unwrap_or(defaults.max_matches_per_file),
        // Legacy field; `casing` overrides it, but keep it coherent anyway.
        smart_case: true,
        casing: Some(casing),
        file_offset,
        page_limit,
        mode,
        time_budget_ms,
        enforce_time_budget,
        before_context,
        after_context,
        classify_definitions,
        trim_whitespace,
        abort_signal: Some(abort),
    }
}

/// Content search.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/grep",
    tag = "grep",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = GrepRequest,
    responses(
        (status = 200, description = "Matching lines", body = GrepResponse),
        (status = 400, description = "Malformed request or cursor", body = crate::error::Problem),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn grep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<GrepRequest>,
) -> ApiResult<Json<GrepResponse>> {
    let ws = workspace(&state, &id)?;
    let file_offset = match &req.cursor {
        Some(c) => c.offset()?,
        None => 0,
    };
    let page_limit = req
        .page_size
        .unwrap_or(state.config.defaults.grep_page_size)
        .max(1);
    let budget = req
        .time_budget_ms
        .unwrap_or(state.config.defaults.grep_time_budget_ms);

    let abort = Arc::new(AtomicBool::new(false));
    // Dropped when this function's future is dropped, i.e. when the client goes away.
    let _cancel = AbortOnDrop(abort.clone());

    let flag = abort.clone();
    let handle = tokio::task::spawn_blocking(move || -> ApiResult<Json<GrepResponse>> {
        let guard = ws.picker.read()?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ApiError::NotReady("index is still building".into()))?;
        let display = ws.root.display.clone();

        let query = parse_with(&req.query, req.preset);
        // Resolve `auto` against the text the engine will actually search for, which
        // excludes constraint tokens.
        let mode = req.mode.resolve(&query.grep_text());

        let result = picker.grep(
            &query,
            &options(
                flag.clone(),
                file_offset,
                page_limit,
                req.casing.into(),
                mode,
                req.max_file_size,
                req.max_matches_per_file,
                req.before_context.unwrap_or(0),
                req.after_context.unwrap_or(0),
                req.classify_definitions,
                req.trim_whitespace,
                budget,
                req.enforce_time_budget,
            ),
        );

        let aborted = flag.load(Ordering::Relaxed);
        Ok(Json(to_response(
            result,
            picker,
            &display,
            mode_name(mode),
            aborted,
        )))
    });

    handle
        .await
        .map_err(|e| ApiError::Internal(format!("grep task failed: {e}")))?
}

/// Multi-pattern content search.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{id}/multi-grep",
    tag = "grep",
    params(("id" = String, Path, description = "Workspace id")),
    request_body = MultiGrepRequest,
    responses(
        (status = 200, description = "Matching lines", body = GrepResponse),
        (status = 400, description = "No patterns, or a malformed cursor", body = crate::error::Problem),
        (status = 404, description = "No such workspace", body = crate::error::Problem),
        (status = 503, description = "Index not ready yet", body = crate::error::Problem),
    ),
)]
pub async fn multi_grep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ProblemJson(req): ProblemJson<MultiGrepRequest>,
) -> ApiResult<Json<GrepResponse>> {
    let ws = workspace(&state, &id)?;

    if req.patterns.is_empty() {
        return Err(ApiError::InvalidBody(
            "patterns must contain at least one entry".into(),
        ));
    }
    if let Some(empty) = req.patterns.iter().position(|p| p.is_empty()) {
        return Err(ApiError::InvalidBody(format!(
            "patterns[{empty}] is empty; an empty pattern would match every line"
        )));
    }

    let file_offset = match &req.cursor {
        Some(c) => c.offset()?,
        None => 0,
    };
    let page_limit = req
        .page_size
        .unwrap_or(state.config.defaults.grep_page_size)
        .max(1);
    let budget = req
        .time_budget_ms
        .unwrap_or(state.config.defaults.grep_time_budget_ms);

    let abort = Arc::new(AtomicBool::new(false));
    let _cancel = AbortOnDrop(abort.clone());

    let flag = abort.clone();
    let handle = tokio::task::spawn_blocking(move || -> ApiResult<Json<GrepResponse>> {
        let guard = ws.picker.read()?;
        let picker = guard
            .as_ref()
            .ok_or_else(|| ApiError::NotReady("index is still building".into()))?;
        let display = ws.root.display.clone();

        let patterns: Vec<&str> = req.patterns.iter().map(String::as_str).collect();
        let constraints = parse_constraints(&req.constraints, req.preset);

        let result = picker.multi_grep(
            &patterns,
            &constraints,
            &options(
                flag.clone(),
                file_offset,
                page_limit,
                req.casing.into(),
                // Multi-pattern search is always literal: that is what Aho-Corasick does.
                fff_search::grep::GrepMode::PlainText,
                req.max_file_size,
                req.max_matches_per_file,
                req.before_context.unwrap_or(0),
                req.after_context.unwrap_or(0),
                req.classify_definitions,
                req.trim_whitespace,
                budget,
                req.enforce_time_budget,
            ),
        );

        let aborted = flag.load(Ordering::Relaxed);
        Ok(Json(to_response(
            result, picker, &display, "plain", aborted,
        )))
    });

    handle
        .await
        .map_err(|e| ApiError::Internal(format!("multi-grep task failed: {e}")))?
}

fn mode_name(mode: fff_search::grep::GrepMode) -> &'static str {
    match mode {
        fff_search::grep::GrepMode::PlainText => "plain",
        fff_search::grep::GrepMode::Regex => "regex",
        fff_search::grep::GrepMode::Fuzzy => "fuzzy",
    }
}

/// `GrepModeDto` is a wire type; this keeps the mapping honest in one place.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::grep::GrepModeDto;

    #[test]
    fn mode_names_match_the_wire_enum() {
        use fff_search::grep::GrepMode;
        assert_eq!(mode_name(GrepMode::PlainText), "plain");
        assert_eq!(mode_name(GrepMode::Regex), "regex");
        assert_eq!(mode_name(GrepMode::Fuzzy), "fuzzy");
        // And the dto resolves to modes whose names are all covered above.
        for dto in [
            GrepModeDto::Plain,
            GrepModeDto::Regex,
            GrepModeDto::Fuzzy,
            GrepModeDto::Auto,
        ] {
            let _ = mode_name(dto.resolve("x"));
        }
    }

    #[test]
    fn abort_guard_trips_the_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _guard = AbortOnDrop(flag.clone());
            assert!(!flag.load(Ordering::Relaxed));
        }
        assert!(
            flag.load(Ordering::Relaxed),
            "dropping the guard must signal the engine to stop"
        );
    }
}
