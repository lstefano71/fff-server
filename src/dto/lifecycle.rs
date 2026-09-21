use fff_query_parser::{Constraint, FFFQuery, FuzzyQuery, GitStatusFilter};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::file::LocationDto;

/// Result of a git-status refresh.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitRefreshResponse {
    /// Files whose cached status changed.
    pub updated: usize,
}

/// Report that a file was opened, so its access-frecency score rises.
///
/// This is the half of frecency a REST server cannot observe for itself. The modification
/// half is computed from mtime and git status and works unaided; the access half needs a
/// client to say what was actually opened. A client that never calls this loses nothing
/// relative to running without the databases.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackAccessRequest {
    /// Repo-relative (forward or back slashes) or absolute. Must resolve inside the
    /// workspace root.
    #[schema(example = "src/main.rs")]
    pub path: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackAccessResponse {
    /// The path as recorded, repo-relative with forward slashes.
    pub relative_path: String,
    /// Times this file has been reported, after this call.
    pub access_count: usize,
}

/// Report which result a query led to, feeding the combo boost.
///
/// The boost only fires once the same (query, file) pair has been reported
/// `minComboCount` times (default 3), so a single call changes nothing visible.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackQueryRequest {
    pub query: String,
    /// The file the client acted on. Repo-relative or absolute.
    pub selected_path: String,
}

/// A query from this workspace's history.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    /// `null` when the history is shorter than the requested offset.
    pub query: Option<String>,
    pub offset: usize,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryQuery {
    /// 0 is the most recent. The ring buffer holds 128 entries.
    #[serde(default)]
    pub offset: usize,
}

/// Which preset to parse with. The presets differ in which constraint kinds they recognise,
/// so the same string can mean different things.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ParsePreset {
    #[default]
    FileSearch,
    DirSearch,
    MixedSearch,
    Grep,
    AiGrep,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParseQueryRequest {
    #[schema(example = "git:modified src/**/*.rs !tests/ user controller")]
    pub query: String,
    #[serde(default)]
    pub preset: ParsePreset,
}

/// What kind of thing a constraint constrains. Also the discriminator value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintType {
    Extension,
    Glob,
    Parts,
    Text,
    Exclude,
    PathSegment,
    FilePath,
    FileType,
    GitStatus,
}

/// The git status categories the engine can filter on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum GitStatusValue {
    Modified,
    Untracked,
    Staged,
    Unmodified,
}

impl From<GitStatusFilter> for GitStatusValue {
    fn from(g: GitStatusFilter) -> Self {
        match g {
            GitStatusFilter::Modified => Self::Modified,
            GitStatusFilter::Untracked => Self::Untracked,
            GitStatusFilter::Staged => Self::Staged,
            GitStatusFilter::Unmodified => Self::Unmodified,
        }
    }
}

/// A file extension, without a dot.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionConstraint {
    /// Always `extension`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub value: String,
}

/// A glob pattern. Forward slashes only.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GlobConstraint {
    /// Always `glob`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub pattern: String,
}

/// Several text parts, all of which must match.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PartsConstraint {
    /// Always `parts`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub values: Vec<String>,
}

/// Literal text a path must contain.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextConstraint {
    /// Always `text`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub value: String,
}

/// Terms to exclude.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExcludeConstraint {
    /// Always `exclude`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub values: Vec<String>,
}

/// A path segment a result must sit under.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PathSegmentConstraint {
    /// Always `pathSegment`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub segment: String,
}

/// A repo-relative file path suffix.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FilePathConstraint {
    /// Always `filePath`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub path: String,
}

/// One of the engine's file-type names, e.g. `rust`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileTypeConstraint {
    /// Always `fileType`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub name: String,
}

/// A git status category.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusConstraintDto {
    /// Always `gitStatus`. The discriminator.
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True when the constraint was negated, as in `!tests/`.
    pub negated: bool,
    pub status: GitStatusValue,
}

/// One parsed constraint, as a discriminated union on `type`.
//
// Each variant is its own named schema so utoipa can emit a real discriminator, and so each
// kind keeps a payload field named for what it is (`pattern`, `segment`, `path`, `status`)
// rather than a generic `value`. `untagged` because the variant structs carry `type`
// themselves, as the OpenAPI discriminator object requires.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
#[schema(discriminator(property_name = "type", mapping(
    ("extension" = "#/components/schemas/ExtensionConstraint"),
    ("glob" = "#/components/schemas/GlobConstraint"),
    ("parts" = "#/components/schemas/PartsConstraint"),
    ("text" = "#/components/schemas/TextConstraint"),
    ("exclude" = "#/components/schemas/ExcludeConstraint"),
    ("pathSegment" = "#/components/schemas/PathSegmentConstraint"),
    ("filePath" = "#/components/schemas/FilePathConstraint"),
    ("fileType" = "#/components/schemas/FileTypeConstraint"),
    ("gitStatus" = "#/components/schemas/GitStatusConstraintDto"),
)))]
pub enum ConstraintDto {
    Extension(ExtensionConstraint),
    Glob(GlobConstraint),
    Parts(PartsConstraint),
    Text(TextConstraint),
    Exclude(ExcludeConstraint),
    PathSegment(PathSegmentConstraint),
    FilePath(FilePathConstraint),
    FileType(FileTypeConstraint),
    GitStatus(GitStatusConstraintDto),
}

impl ConstraintDto {
    pub fn build(c: &Constraint<'_>) -> Self {
        let mut negated = false;
        let mut current = c;
        // Unwrap however many Not layers the parser produced, toggling as we go, so double
        // negation is reported accurately rather than collapsed to "negated".
        while let Constraint::Not(inner) = current {
            negated = !negated;
            current = inner;
        }

        match current {
            Constraint::Extension(v) => Self::Extension(ExtensionConstraint {
                constraint_type: ConstraintType::Extension,
                negated,
                value: (*v).to_owned(),
            }),
            Constraint::Glob(p) => Self::Glob(GlobConstraint {
                constraint_type: ConstraintType::Glob,
                negated,
                pattern: (*p).to_owned(),
            }),
            Constraint::Parts(vs) => Self::Parts(PartsConstraint {
                constraint_type: ConstraintType::Parts,
                negated,
                values: vs.iter().map(|v| (*v).to_owned()).collect(),
            }),
            Constraint::Text(v) => Self::Text(TextConstraint {
                constraint_type: ConstraintType::Text,
                negated,
                value: (*v).to_owned(),
            }),
            Constraint::Exclude(vs) => Self::Exclude(ExcludeConstraint {
                constraint_type: ConstraintType::Exclude,
                negated,
                values: vs.iter().map(|v| (*v).to_owned()).collect(),
            }),
            Constraint::PathSegment(s) => Self::PathSegment(PathSegmentConstraint {
                constraint_type: ConstraintType::PathSegment,
                negated,
                segment: (*s).to_owned(),
            }),
            Constraint::FilePath(p) => Self::FilePath(FilePathConstraint {
                constraint_type: ConstraintType::FilePath,
                negated,
                path: (*p).to_owned(),
            }),
            Constraint::FileType(n) => Self::FileType(FileTypeConstraint {
                constraint_type: ConstraintType::FileType,
                negated,
                name: (*n).to_owned(),
            }),
            Constraint::GitStatus(g) => Self::GitStatus(GitStatusConstraintDto {
                constraint_type: ConstraintType::GitStatus,
                negated,
                status: GitStatusValue::from(*g),
            }),
            // Unreachable: every Not was unwrapped above. Represented rather than panicking,
            // because Constraint is an external type that may gain variants.
            Constraint::Not(_) => Self::Text(TextConstraint {
                constraint_type: ConstraintType::Text,
                negated,
                value: String::new(),
            }),
        }
    }
}

/// How the parser decomposed a query.
///
/// Exists because the DSL is powerful and silent about mistakes: a token that looks like a
/// glob but is not recognised as one simply becomes fuzzy text, and the results still look
/// plausible. This endpoint makes that visible from the client side.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParseQueryResponse {
    /// Echoed back, byte-for-byte.
    pub query: String,
    pub constraints: Vec<ConstraintDto>,
    /// The fuzzy terms left after constraints were removed.
    pub fuzzy_parts: Vec<String>,
    /// What grep would search for: the query minus constraint tokens.
    pub grep_text: String,
    /// A trailing `file.ts:42:10`, if present.
    pub location: Option<LocationDto>,
    /// Problems that would otherwise pass unnoticed.
    pub warnings: Vec<String>,
}

impl ParseQueryResponse {
    pub fn build(query: &str, parsed: &FFFQuery<'_>, warnings: Vec<String>) -> Self {
        let fuzzy_parts = match &parsed.fuzzy_query {
            FuzzyQuery::Empty => Vec::new(),
            FuzzyQuery::Text(t) => vec![(*t).to_owned()],
            FuzzyQuery::Parts(parts) => parts.iter().map(|p| (*p).to_owned()).collect(),
        };
        Self {
            query: query.to_owned(),
            constraints: parsed
                .constraints
                .iter()
                .map(ConstraintDto::build)
                .collect(),
            fuzzy_parts,
            grep_text: parsed.grep_text(),
            location: parsed.location.map(Into::into),
            warnings,
        }
    }
}
