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

/// What kind of thing a constraint constrains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintType {
    /// `value` holds the extension, without a dot.
    Extension,
    /// `value` holds the glob pattern.
    Glob,
    /// `values` holds the text parts.
    Parts,
    /// `value` holds the literal text.
    Text,
    /// `values` holds the excluded terms.
    Exclude,
    /// `value` holds the path segment.
    PathSegment,
    /// `value` holds the repo-relative file path.
    FilePath,
    /// `value` holds the engine's file-type name.
    FileType,
    /// `value` is one of `modified`, `untracked`, `staged`, `unmodified`.
    GitStatus,
}

/// One parsed constraint. `type` says which field carries the payload.
//
// Flat rather than a discriminated union, for two reasons found the hard way. Mirroring
// the engine's `Not(Box<Constraint>)` made this type recursive, which sent utoipa's schema
// generation into infinite recursion and overflowed the stack at startup. Replacing that
// with a `oneOf` plus a flattened sibling field then produced a schema Kiota refuses to
// generate from at all: an `allOf` over an anonymous `oneOf`, with no discriminator and no
// named variants to map one to. See DESIGN.md.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintDto {
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub constraint_type: ConstraintType,
    /// True for a negated constraint such as `!tests/`. Nested negation is collapsed, so
    /// `Not(Not(x))` reports the inner constraint with `negated: false` rather than losing a
    /// level.
    pub negated: bool,
    /// The single-valued payload, for every type except `parts` and `exclude`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The multi-valued payload, for `parts` and `exclude`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

impl ConstraintDto {
    pub fn build(c: &Constraint<'_>) -> Self {
        let mut negated = false;
        let mut current = c;
        // Unwrap however many Not layers the parser produced, toggling as we go, so double
        // negation is reported accurately rather than flattened to "negated".
        while let Constraint::Not(inner) = current {
            negated = !negated;
            current = inner;
        }

        let single = |t: ConstraintType, v: &str| Self {
            constraint_type: t,
            negated,
            value: Some(v.to_owned()),
            values: Vec::new(),
        };
        let multi = |t: ConstraintType, vs: &[&str]| Self {
            constraint_type: t,
            negated,
            value: None,
            values: vs.iter().map(|v| (*v).to_owned()).collect(),
        };

        match current {
            Constraint::Extension(v) => single(ConstraintType::Extension, v),
            Constraint::Glob(p) => single(ConstraintType::Glob, p),
            Constraint::Parts(vs) => multi(ConstraintType::Parts, vs),
            Constraint::Text(v) => single(ConstraintType::Text, v),
            Constraint::Exclude(vs) => multi(ConstraintType::Exclude, vs),
            Constraint::PathSegment(s) => single(ConstraintType::PathSegment, s),
            Constraint::FilePath(p) => single(ConstraintType::FilePath, p),
            Constraint::FileType(n) => single(ConstraintType::FileType, n),
            Constraint::GitStatus(g) => single(
                ConstraintType::GitStatus,
                match g {
                    GitStatusFilter::Modified => "modified",
                    GitStatusFilter::Untracked => "untracked",
                    GitStatusFilter::Staged => "staged",
                    GitStatusFilter::Unmodified => "unmodified",
                },
            ),
            // Unreachable: every Not was unwrapped above. Represented rather than panicking,
            // because Constraint is an external type that may gain variants.
            Constraint::Not(_) => single(ConstraintType::Text, ""),
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
