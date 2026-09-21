use fff_search::grep::{Casing, GrepMode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::file::{FileItemDto, Utf16Range};
use crate::error::ApiError;

/// Case handling. 0.11 replaced the old `smart_case` boolean with this; the boolean remains
/// only as a legacy fallback and is not offered here.
///
/// Unlike `matchType`, this is a real enum in the engine, so closing it in the contract is
/// safe and gives the C# client a proper enum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum CasingDto {
    /// Case-insensitive unless the pattern contains an uppercase character.
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

impl From<CasingDto> for Casing {
    fn from(c: CasingDto) -> Self {
        match c {
            CasingDto::Smart => Self::Smart,
            CasingDto::Sensitive => Self::Sensitive,
            CasingDto::Insensitive => Self::Insensitive,
        }
    }
}

/// How to interpret the pattern.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum GrepModeDto {
    /// Literal text, SIMD-accelerated. The fastest path.
    #[default]
    Plain,
    /// The same regex engine ripgrep uses. An uncompilable pattern falls back to literal
    /// matching and reports why in `regexFallbackError` rather than failing the request.
    Regex,
    /// Typo-tolerant Smith-Waterman scoring per line. Substantially slower, and much slower
    /// still on a workspace whose content index has not finished building.
    Fuzzy,
    /// Plain unless the pattern contains regex metacharacters, then regex. This is what the
    /// MCP server does, via the engine's own `has_regex_metacharacters`.
    Auto,
}

impl GrepModeDto {
    /// `Auto` is resolved against the pattern the engine will actually search for, which is
    /// the query minus any constraint tokens.
    pub fn resolve(self, grep_text: &str) -> GrepMode {
        match self {
            Self::Plain => GrepMode::PlainText,
            Self::Regex => GrepMode::Regex,
            Self::Fuzzy => GrepMode::Fuzzy,
            Self::Auto => {
                if fff_search::has_regex_metacharacters(grep_text) {
                    GrepMode::Regex
                } else {
                    GrepMode::PlainText
                }
            }
        }
    }
}

/// Which parser preset to read constraint tokens with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum GrepPreset {
    /// Recognises extension, glob, path and exclusion constraints.
    #[default]
    Grep,
    /// Additionally treats a filename-looking token (`score.rs`) as a path filter that
    /// scopes the search. Convenient for agents, surprising for anyone who meant it as
    /// literal text.
    AiGrep,
}

/// An opaque continuation token.
///
/// It wraps the engine's file offset, which is a forward-only position in the filtered file
/// list. It is deliberately opaque: there is no way to seek backwards or jump to page N, so
/// exposing it as a page number would be a promise the engine cannot keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
// Declared as a plain string in the contract. Without this utoipa emits a wrapper schema
// that Kiota misreads as a polymorphic type ("Discriminator GrepCursor is not inherited
// from GrepCursor").
#[schema(value_type = String, example = "NTQ0")]
pub struct GrepCursor(String);

impl GrepCursor {
    pub fn from_offset(offset: usize) -> Self {
        // Base64url of the decimal offset. Not secret, just not something to build by hand -
        // the encoding can change without breaking clients that round-trip it.
        Self(base64_encode(offset.to_string().as_bytes()))
    }

    pub fn offset(&self) -> Result<usize, ApiError> {
        let bytes = base64_decode(&self.0)
            .ok_or_else(|| ApiError::InvalidBody("cursor is not a valid token".into()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ApiError::InvalidBody("cursor is not a valid token".into()))?;
        text.parse::<usize>()
            .map_err(|_| ApiError::InvalidBody("cursor is not a valid token".into()))
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64[(n & 63) as usize] as char);
        }
    }
    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits = Vec::with_capacity(input.len());
    for c in input.bytes() {
        bits.push(B64.iter().position(|&b| b == c)? as u32);
    }
    let mut out = Vec::new();
    for chunk in bits.chunks(4) {
        let mut n = 0u32;
        for (i, v) in chunk.iter().enumerate() {
            n |= v << (18 - 6 * i as u32);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Content search request.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrepRequest {
    /// Pattern plus optional constraint tokens, e.g. `*.rs !tests/ TODO`. Passed to the
    /// engine byte-for-byte: backslashes are **not** folded, because a backslash is
    /// legitimate content in a pattern.
    #[schema(example = "*.rs TODO")]
    pub query: String,
    #[serde(default)]
    pub mode: GrepModeDto,
    #[serde(default)]
    pub casing: CasingDto,
    #[serde(default)]
    pub preset: GrepPreset,

    /// Continuation token from a previous response's `nextCursor`.
    #[schema(value_type = Option<String>)]
    pub cursor: Option<GrepCursor>,
    /// Soft cap on matches per response; the engine finishes the file it is in.
    pub page_size: Option<usize>,

    pub max_file_size: Option<u64>,
    pub max_matches_per_file: Option<usize>,
    pub before_context: Option<usize>,
    pub after_context: Option<usize>,
    /// Tags lines that look like definitions. Roughly 2% overhead.
    #[serde(default)]
    pub classify_definitions: bool,
    /// Strips leading whitespace from matched and context lines, adjusting the reported
    /// offsets accordingly.
    #[serde(default)]
    pub trim_whitespace: bool,

    /// Milliseconds before returning partial results. 0, the default, means unbounded.
    /// Grep over SMB measured ~7 ms/file, so a full 87k-file tree is around ten minutes: a
    /// budget here is a real safeguard, but one that silently truncates, which is why
    /// cursor paging is the better tool.
    pub time_budget_ms: Option<u64>,
    /// Apply the budget even before anything has matched. Off by default, matching the
    /// engine: a zero-match query otherwise scans everything.
    #[serde(default)]
    pub enforce_time_budget: bool,
}

/// Multi-pattern search: any pattern matching counts, via SIMD Aho-Corasick. Faster than
/// regex alternation and much faster than N separate searches. Always literal text.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MultiGrepRequest {
    /// At least one literal pattern.
    #[schema(example = json!(["TODO", "FIXME", "XXX"]))]
    pub patterns: Vec<String>,
    /// Constraint tokens only — no pattern text. e.g. `*.rs !tests/`.
    #[serde(default)]
    pub constraints: String,
    #[serde(default)]
    pub casing: CasingDto,
    #[serde(default)]
    pub preset: GrepPreset,

    #[schema(value_type = Option<String>)]
    pub cursor: Option<GrepCursor>,
    pub page_size: Option<usize>,
    pub max_file_size: Option<u64>,
    pub max_matches_per_file: Option<usize>,
    pub before_context: Option<usize>,
    pub after_context: Option<usize>,
    #[serde(default)]
    pub classify_definitions: bool,
    #[serde(default)]
    pub trim_whitespace: bool,
    pub time_budget_ms: Option<u64>,
    #[serde(default)]
    pub enforce_time_budget: bool,
}

/// One matching line.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrepMatchDto {
    /// Index into the response's `files` array. Kept as an index rather than embedding the
    /// file, so a file's metadata is not repeated across hundreds of matches within it.
    pub file_index: usize,
    /// 1-based.
    pub line_number: u64,
    /// Column of the first match on this line, in **UTF-16 code units** into `lineContent`.
    pub col_utf16: u32,
    /// Byte offset of this line within the file. Stays a byte count, because that is what it
    /// is for: seeking directly to the line.
    pub byte_offset: u64,
    pub line_content: String,
    /// Every match on this line, in UTF-16 code units into `lineContent`. The MCP server
    /// uses these only to centre its truncation window and never emits them.
    pub match_ranges_utf16: Vec<Utf16Range>,
    /// Only present in fuzzy mode.
    pub fuzzy_score: Option<u16>,
    /// Only meaningful when `classifyDefinitions` was requested.
    pub is_definition: bool,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GrepResponse {
    pub matches: Vec<GrepMatchDto>,
    /// Deduplicated files referenced by `matches[].fileIndex`.
    pub files: Vec<FileItemDto>,

    pub total_matched: usize,
    /// Files actually searched in this call.
    pub total_files_searched: usize,
    /// Files in the index.
    pub total_files: usize,
    /// Files eligible after filtering out binaries, oversized files and constraint misses.
    pub filtered_file_count: usize,
    pub files_with_matches: usize,

    /// Pass back as `cursor` for the next page. `null` means the search is exhausted.
    #[schema(value_type = Option<String>)]
    pub next_cursor: Option<GrepCursor>,
    /// Set when a regex failed to compile and the search fell back to literal matching.
    /// The request still succeeds; this explains why the results look literal.
    pub regex_fallback_error: Option<String>,
    /// True when a constrained query found nothing and the engine retried the whole raw
    /// query as literal text, ignoring the constraints it had inferred.
    pub literal_fallback: bool,
    /// The mode actually used, after `auto` resolution.
    pub mode: &'static str,
    /// True when the engine's abort signal was tripped, which currently means the client
    /// disconnected mid-search. Results are partial.
    ///
    /// Note this does **not** cover a `timeBudgetMs` truncation: the engine reports that by
    /// returning early with a non-null `nextCursor` and no separate flag, so a client that
    /// cares about completeness should page until `nextCursor` is null rather than trust
    /// this field alone.
    pub aborted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip() {
        for offset in [0usize, 1, 7, 544, 86_936, usize::from(u16::MAX)] {
            let c = GrepCursor::from_offset(offset);
            assert_eq!(c.offset().unwrap(), offset, "round trip for {offset}");
        }
    }

    #[test]
    fn cursors_are_opaque_not_the_bare_number() {
        // Opaque so the encoding can change later without breaking clients that only
        // round-trip the value.
        let c = GrepCursor::from_offset(544);
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("544"), "cursor leaked its offset: {json}");
    }

    #[test]
    fn a_corrupt_cursor_is_a_bad_request() {
        let c: GrepCursor = serde_json::from_str("\"!!!not-base64!!!\"").unwrap();
        let err = c.offset().unwrap_err();
        assert_eq!(err.problem().status, 400);
    }

    #[test]
    fn auto_mode_picks_regex_only_for_metacharacters() {
        assert_eq!(GrepModeDto::Auto.resolve("TODO"), GrepMode::PlainText);
        assert_eq!(GrepModeDto::Auto.resolve("fn \\w+"), GrepMode::Regex);
        // Explicit modes are never second-guessed.
        assert_eq!(GrepModeDto::Plain.resolve("fn \\w+"), GrepMode::PlainText);
    }
}
