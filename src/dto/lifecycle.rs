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

/// What a constraint constrains, tagged by kind.
///
/// Deliberately **not** recursive. The engine models negation as `Not(Box<Constraint>)`, but
/// mirroring that here made `ConstraintDto` self-referential, which sent utoipa's schema
/// generation into infinite recursion and overflowed the stack at startup. Negation is a
/// sibling boolean instead — which is also a far easier shape to consume in C# than a
/// recursive union.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConstraintKind {
    Extension { value: String },
    Glob { pattern: String },
    Parts { values: Vec<String> },
    Text { value: String },
    Exclude { values: Vec<String> },
    PathSegment { segment: String },
    FilePath { path: String },
    FileType { name: String },
    GitStatus { status: &'static str },
}

/// One parsed constraint.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintDto {
    #[serde(flatten)]
    pub kind: ConstraintKind,
    /// True for a negated constraint such as `!tests/`. Nested negation is collapsed, so
    /// `Not(Not(x))` reports `x` with `negated: false` rather than losing a level.
    pub negated: bool,
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

        let kind = match current {
            Constraint::Extension(v) => ConstraintKind::Extension {
                value: (*v).to_owned(),
            },
            Constraint::Glob(p) => ConstraintKind::Glob {
                pattern: (*p).to_owned(),
            },
            Constraint::Parts(vs) => ConstraintKind::Parts {
                values: vs.iter().map(|v| (*v).to_owned()).collect(),
            },
            Constraint::Text(v) => ConstraintKind::Text {
                value: (*v).to_owned(),
            },
            Constraint::Exclude(vs) => ConstraintKind::Exclude {
                values: vs.iter().map(|v| (*v).to_owned()).collect(),
            },
            Constraint::PathSegment(s) => ConstraintKind::PathSegment {
                segment: (*s).to_owned(),
            },
            Constraint::FilePath(p) => ConstraintKind::FilePath {
                path: (*p).to_owned(),
            },
            Constraint::FileType(n) => ConstraintKind::FileType {
                name: (*n).to_owned(),
            },
            Constraint::GitStatus(g) => ConstraintKind::GitStatus {
                status: match g {
                    GitStatusFilter::Modified => "modified",
                    GitStatusFilter::Untracked => "untracked",
                    GitStatusFilter::Staged => "staged",
                    GitStatusFilter::Unmodified => "unmodified",
                },
            },
            // Unreachable: every Not was unwrapped above. Represented rather than panicking,
            // because Constraint is an external type that may gain variants.
            Constraint::Not(_) => ConstraintKind::Text {
                value: String::new(),
            },
        };

        Self { kind, negated }
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
