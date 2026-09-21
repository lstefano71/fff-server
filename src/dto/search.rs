use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::file::{DirItemDto, FileItemDto, LocationDto, ScoreDto, Utf16Range};
use crate::query::StructuredQuery;

/// Search request. Supply `query`, or `structured`, or neither (an empty query ranks the
/// whole index by frecency). Supplying both is rejected rather than silently preferring one.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchRequest {
    /// The raw fff query DSL, e.g. `git:modified src/**/*.rs !tests/ user controller`.
    /// Passed to the engine byte-for-byte. Glob separators must be `/`.
    #[schema(example = "src/**/*.rs button")]
    pub query: Option<String>,
    /// Constraints as data, instead of a DSL string.
    pub structured: Option<StructuredQuery>,

    /// Zero-based page index.
    pub page: Option<usize>,
    /// Defaults to `defaults.page_size`.
    pub page_size: Option<usize>,

    /// A path to deprioritise, so the file a caller is already looking at does not dominate
    /// its own results. Also feeds the distance bonus for nearby paths.
    pub current_file: Option<String>,
    /// 0 lets the engine choose.
    pub max_threads: Option<usize>,
    /// Query-history combo boost. Only has an effect once a client reports selections via
    /// `track-query`.
    pub combo_boost_multiplier: Option<i32>,
    pub min_combo_count: Option<u32>,
}

/// Glob-only request. Bypasses the query parser entirely: the pattern is used literally,
/// results are frecency-ranked. Use it when the pattern is already a glob and fuzzy
/// matching on top would only add noise.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobRequest {
    /// Forward slashes only — the matcher does not treat `\` as a separator.
    #[schema(example = "**/*.rs")]
    pub pattern: String,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
    pub current_file: Option<String>,
    pub max_threads: Option<usize>,
}

/// One file hit, with its score and match highlighting.
///
/// The engine returns items, scores and match offsets as three parallel arrays; they are
/// zipped here. Nothing is lost, and a generated C# client is far better for it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub item: FileItemDto,
    pub score: ScoreDto,
    /// Which parts of `item.relativePath` matched, in UTF-16 code units. The MCP server
    /// discards this information entirely.
    pub match_ranges_utf16: Vec<Utf16Range>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirHit {
    pub item: DirItemDto,
    pub score: ScoreDto,
}

/// Whether a mixed hit is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum MixedKind {
    File,
    Directory,
}

/// A file from a mixed search.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MixedFileHit {
    /// Always `file`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub kind: MixedKind,
    pub item: FileItemDto,
    pub score: ScoreDto,
}

/// A directory from a mixed search.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MixedDirectoryHit {
    /// Always `directory`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub kind: MixedKind,
    pub item: DirItemDto,
    pub score: ScoreDto,
}

/// A hit from a mixed search: a file or a directory, distinguished by `type`.
//
// A genuine discriminated union, which utoipa will only emit for an enum whose variants are
// newtypes over *named* schemas - inline variants give an anonymous `oneOf` that no generator
// can map a discriminator onto. `untagged` because each variant struct carries the `type`
// property itself, which is what the OpenAPI discriminator object requires.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
#[schema(discriminator(property_name = "type", mapping(
    ("file" = "#/components/schemas/MixedFileHit"),
    ("directory" = "#/components/schemas/MixedDirectoryHit"),
)))]
pub enum MixedHit {
    File(MixedFileHit),
    Directory(MixedDirectoryHit),
}

impl MixedHit {
    pub fn file(item: FileItemDto, score: ScoreDto) -> Self {
        Self::File(MixedFileHit {
            kind: MixedKind::File,
            item,
            score,
        })
    }

    pub fn directory(item: DirItemDto, score: ScoreDto) -> Self {
        Self::Directory(MixedDirectoryHit {
            kind: MixedKind::Directory,
            item,
            score,
        })
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub items: Vec<SearchHit>,
    /// Matches across the whole index, not just this page.
    pub total_matched: usize,
    /// Files in the index.
    pub total_files: usize,
    /// A `file.ts:42:10` suffix parsed out of the query, if present.
    pub location: Option<LocationDto>,
    pub page: usize,
    pub page_size: usize,
    pub has_more: bool,
    /// Non-fatal notes about the query — currently backslashes in glob-looking tokens,
    /// which would otherwise match nothing with no explanation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirSearchResponse {
    pub items: Vec<DirHit>,
    pub total_matched: usize,
    pub total_dirs: usize,
    pub page: usize,
    pub page_size: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MixedSearchResponse {
    pub items: Vec<MixedHit>,
    pub total_matched: usize,
    pub total_files: usize,
    pub total_dirs: usize,
    pub location: Option<LocationDto>,
    pub page: usize,
    pub page_size: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}
