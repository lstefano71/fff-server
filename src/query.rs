//! Query construction.
//!
//! Two ways in, per DESIGN.md:
//!
//! - a **raw DSL string**, which is what the engine's parser consumes and what every other
//!   binding passes through;
//! - **structured constraints**, so a client building a query programmatically need not
//!   concatenate and escape strings.
//!
//! Separator folding (`\` to `/`) is applied **only** to the structured path-ish fields.
//! Folding a raw query would corrupt grep patterns, where a backslash is legitimate content.

use fff_query_parser::{
    Constraint, ConstraintVec, DirSearchConfig, FFFQuery, FileSearchConfig, FuzzyQuery,
    GitStatusFilter, MixedSearchConfig, QueryParser,
};
use serde::Deserialize;
use utoipa::ToSchema;

/// Which `ParserConfig` preset to parse a raw query with. The presets differ in which
/// constraint kinds they recognise, so the wrong one silently changes what a query means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    FileSearch,
    DirSearch,
    MixedSearch,
}

/// Git status filter, mirroring the engine's four categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum GitStatusConstraint {
    Modified,
    Untracked,
    Staged,
    Unmodified,
}

impl From<GitStatusConstraint> for GitStatusFilter {
    fn from(g: GitStatusConstraint) -> Self {
        match g {
            GitStatusConstraint::Modified => Self::Modified,
            GitStatusConstraint::Untracked => Self::Untracked,
            GitStatusConstraint::Staged => Self::Staged,
            GitStatusConstraint::Unmodified => Self::Unmodified,
        }
    }
}

/// Constraints as data, for clients that would rather not build DSL strings.
///
/// Separators are folded in `globs`, `pathSegments`, `filePaths` and `excludePathSegments`,
/// where a separator is unambiguously a separator. The engine's glob backend treats **only**
/// `/` as a separator, so without folding `src\**\*.rs` would not be applied as a glob at
/// all — it would fall through to fuzzy text matching and quietly return unrelated results.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredQuery {
    /// Free-text fuzzy terms. Split on whitespace into parts, as the DSL parser would.
    pub fuzzy: Option<String>,
    /// Glob patterns, e.g. `src/**/*.rs`.
    #[serde(default)]
    pub globs: Vec<String>,
    /// Bare extensions, without a dot: `rs`, `md`.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Path segments a result must contain, e.g. `src`.
    #[serde(default)]
    pub path_segments: Vec<String>,
    /// Exact repo-relative file paths.
    #[serde(default)]
    pub file_paths: Vec<String>,
    /// Engine file-type names, e.g. `rust`.
    #[serde(default)]
    pub file_types: Vec<String>,
    /// Literal text a path must contain.
    #[serde(default)]
    pub text: Vec<String>,
    /// Path segments to exclude.
    #[serde(default)]
    pub exclude_path_segments: Vec<String>,
    /// Literal text to exclude.
    #[serde(default)]
    pub exclude_text: Vec<String>,
    pub git_status: Option<GitStatusConstraint>,
}

/// Owns the strings a `FFFQuery` borrows from, so a handler can build one and keep it alive
/// across the search call.
pub struct PreparedQuery {
    raw: String,
    preset: Preset,
    structured: Option<OwnedStructured>,
}

struct OwnedStructured {
    fuzzy_parts: Vec<String>,
    globs: Vec<String>,
    extensions: Vec<String>,
    path_segments: Vec<String>,
    file_paths: Vec<String>,
    file_types: Vec<String>,
    text: Vec<String>,
    exclude_path_segments: Vec<String>,
    exclude_text: Vec<String>,
    git_status: Option<GitStatusFilter>,
}

impl PreparedQuery {
    /// A raw DSL string, parsed with `preset`. Left byte-for-byte untouched.
    pub fn raw(query: &str, preset: Preset) -> Self {
        Self {
            raw: query.to_owned(),
            preset,
            structured: None,
        }
    }

    /// Structured constraints. Path-ish fields have their separators folded.
    pub fn structured(q: &StructuredQuery, preset: Preset) -> Self {
        Self {
            raw: String::new(),
            preset,
            structured: Some(OwnedStructured {
                fuzzy_parts: q
                    .fuzzy
                    .as_deref()
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect(),
                globs: fold_all(&q.globs),
                extensions: q.extensions.clone(),
                path_segments: fold_all(&q.path_segments),
                file_paths: fold_all(&q.file_paths),
                file_types: q.file_types.clone(),
                text: q.text.clone(),
                exclude_path_segments: fold_all(&q.exclude_path_segments),
                exclude_text: q.exclude_text.clone(),
                git_status: q.git_status.map(Into::into),
            }),
        }
    }

    /// Borrows from `self`, so the returned query must not outlive it.
    pub fn query(&self) -> FFFQuery<'_> {
        match &self.structured {
            None => match self.preset {
                Preset::FileSearch => QueryParser::new(FileSearchConfig).parse(&self.raw),
                Preset::DirSearch => QueryParser::new(DirSearchConfig).parse(&self.raw),
                Preset::MixedSearch => QueryParser::new(MixedSearchConfig).parse(&self.raw),
            },
            Some(s) => {
                let mut constraints = ConstraintVec::new();
                for g in &s.globs {
                    constraints.push(Constraint::Glob(g));
                }
                for e in &s.extensions {
                    constraints.push(Constraint::Extension(e));
                }
                for p in &s.path_segments {
                    constraints.push(Constraint::PathSegment(p));
                }
                for f in &s.file_paths {
                    constraints.push(Constraint::FilePath(f));
                }
                for t in &s.file_types {
                    constraints.push(Constraint::FileType(t));
                }
                for t in &s.text {
                    constraints.push(Constraint::Text(t));
                }
                for p in &s.exclude_path_segments {
                    constraints.push(Constraint::Not(Box::new(Constraint::PathSegment(p))));
                }
                for t in &s.exclude_text {
                    constraints.push(Constraint::Not(Box::new(Constraint::Text(t))));
                }
                if let Some(g) = s.git_status {
                    constraints.push(Constraint::GitStatus(g));
                }

                let fuzzy_query = match s.fuzzy_parts.len() {
                    0 => FuzzyQuery::Empty,
                    1 => FuzzyQuery::Text(&s.fuzzy_parts[0]),
                    _ => FuzzyQuery::Parts(s.fuzzy_parts.iter().map(String::as_str).collect()),
                };

                FFFQuery {
                    raw_query: &self.raw,
                    constraints,
                    fuzzy_query,
                    location: None,
                }
            }
        }
    }
}

/// Warnings about a query that would otherwise fail silently.
pub fn warnings_for(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in raw.split_whitespace() {
        let looks_glob = token.contains('*') || token.contains('?') || token.contains('[');
        if looks_glob && token.contains('\\') {
            out.push(format!(
                "glob-looking token {token:?} contains a backslash. The glob matcher treats \
                 only '/' as a separator, so this token is not applied as a glob at all - it \
                 falls through to fuzzy text matching, which usually returns unrelated \
                 results rather than none. Use forward slashes, or pass it in the structured \
                 `globs` field, where separators are folded."
            ));
        }
    }
    out
}

fn fold_all(values: &[String]) -> Vec<String> {
    values.iter().map(|v| v.replace('\\', "/")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_queries_are_not_folded() {
        // A grep pattern may legitimately contain backslashes; folding would corrupt it.
        let q = PreparedQuery::raw(r"\r\n", Preset::FileSearch);
        assert_eq!(q.raw, r"\r\n");
    }

    #[test]
    fn structured_path_fields_are_folded() {
        let s = StructuredQuery {
            globs: vec![r"src\**\*.rs".into()],
            path_segments: vec![r"src\core".into()],
            ..Default::default()
        };
        let q = PreparedQuery::structured(&s, Preset::FileSearch);
        let owned = q.structured.as_ref().unwrap();
        assert_eq!(owned.globs[0], "src/**/*.rs");
        assert_eq!(owned.path_segments[0], "src/core");
    }

    #[test]
    fn structured_text_is_not_folded() {
        let s = StructuredQuery {
            text: vec![r"C:\Windows".into()],
            ..Default::default()
        };
        let q = PreparedQuery::structured(&s, Preset::FileSearch);
        assert_eq!(q.structured.as_ref().unwrap().text[0], r"C:\Windows");
    }

    #[test]
    fn structured_builds_the_expected_constraints() {
        let s = StructuredQuery {
            fuzzy: Some("user controller".into()),
            globs: vec!["src/**/*.rs".into()],
            extensions: vec!["rs".into()],
            exclude_path_segments: vec!["tests".into()],
            git_status: Some(GitStatusConstraint::Modified),
            ..Default::default()
        };
        let prepared = PreparedQuery::structured(&s, Preset::FileSearch);
        let q = prepared.query();

        assert_eq!(q.constraints.len(), 4);
        assert!(matches!(q.fuzzy_query, FuzzyQuery::Parts(ref p) if p.len() == 2));
        assert!(
            q.constraints
                .iter()
                .any(|c| matches!(c, Constraint::Glob("src/**/*.rs")))
        );
        assert!(q
            .constraints
            .iter()
            .any(|c| matches!(c, Constraint::Not(inner) if matches!(**inner, Constraint::PathSegment("tests")))));
        assert!(
            q.constraints
                .iter()
                .any(|c| matches!(c, Constraint::GitStatus(GitStatusFilter::Modified)))
        );
    }

    #[test]
    fn single_fuzzy_term_is_text_not_parts() {
        let s = StructuredQuery {
            fuzzy: Some("button".into()),
            ..Default::default()
        };
        let prepared = PreparedQuery::structured(&s, Preset::FileSearch);
        assert!(matches!(
            prepared.query().fuzzy_query,
            FuzzyQuery::Text("button")
        ));
    }

    #[test]
    fn raw_dsl_is_parsed_into_constraints() {
        let prepared = PreparedQuery::raw("*.rs button", Preset::FileSearch);
        let q = prepared.query();
        assert!(!q.constraints.is_empty(), "extension constraint expected");
    }

    #[test]
    fn backslash_globs_are_warned_about() {
        let w = warnings_for(r"src\**\*.rs button");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("not applied as a glob"));
    }

    #[test]
    fn ordinary_queries_warn_about_nothing() {
        assert!(warnings_for("src/**/*.rs button").is_empty());
        // A backslash with no glob metacharacter is probably literal text; leave it alone.
        assert!(warnings_for(r"C:\Windows").is_empty());
    }
}
