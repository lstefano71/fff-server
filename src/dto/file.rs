//! File, directory and score wire types.
//!
//! This is where the project's premise is kept: every field the engine computes is carried
//! through, including the three the C ABI and the TypeScript SDK drop (`gitStatusBoost`,
//! `pathAlignmentBonus`, `gitRecencyBoost`), the match ranges the MCP server discards, and
//! the complete git status rather than a single lossy label.

use fff_search::Location;
use fff_search::file_picker::FilePicker;
use fff_search::types::{DirItem, FileItem, Score};
use git2::Status;
use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A half-open range of **UTF-16 code units**.
///
/// The engine produces UTF-8 byte offsets. Those are converted here because .NET strings
/// are UTF-16: a byte offset would silently address the wrong characters on any line
/// containing non-ASCII, and the point of this API is that the client does no
/// post-processing.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct Utf16Range {
    pub start: u32,
    pub end: u32,
}

/// Git status, in full.
///
/// `git2::Status` is a bitflags value — a file can be staged-modified *and* worktree-modified
/// at once. Every other fff binding flattens it to one string and loses that, so both forms
/// are emitted: `status` for convenience, `flags` for fidelity.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GitStatus {
    /// fff's own single-label vocabulary: `clean`, `modified`, `untracked`, `staged_new`,
    /// `staged_modified`, `staged_deleted`, `deleted`, `renamed`, `ignored`, `unknown`.
    #[schema(example = "modified")]
    pub status: &'static str,
    /// Every set bit, so a client can see combinations the single label hides.
    #[schema(example = json!(["stagedModified", "modified"]))]
    pub flags: Vec<&'static str>,
}

impl GitStatus {
    pub fn from_status(status: Option<Status>) -> Self {
        Self {
            status: fff_search::git::format_git_status(status),
            flags: flags_of(status),
        }
    }
}

fn flags_of(status: Option<Status>) -> Vec<&'static str> {
    let Some(s) = status else { return Vec::new() };
    let mut out = Vec::new();
    for (bit, name) in [
        (Status::INDEX_NEW, "stagedNew"),
        (Status::INDEX_MODIFIED, "stagedModified"),
        (Status::INDEX_DELETED, "stagedDeleted"),
        (Status::INDEX_RENAMED, "stagedRenamed"),
        (Status::INDEX_TYPECHANGE, "stagedTypechange"),
        (Status::WT_NEW, "untracked"),
        (Status::WT_MODIFIED, "modified"),
        (Status::WT_DELETED, "deleted"),
        (Status::WT_RENAMED, "renamed"),
        (Status::WT_TYPECHANGE, "typechange"),
        (Status::WT_UNREADABLE, "unreadable"),
        (Status::IGNORED, "ignored"),
        (Status::CONFLICTED, "conflicted"),
    ] {
        if s.contains(bit) {
            out.push(name);
        }
    }
    out
}

/// An indexed file.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileItemDto {
    /// Repo-relative, **forward slashes on every platform**, carrying on-disk casing. This
    /// is what the engine stores and what globs and path constraints match against.
    #[schema(example = "crates/fff-core/src/types.rs")]
    pub relative_path: String,
    pub file_name: String,
    /// Parent directory, with a trailing `/`. Empty at the root.
    pub directory: String,
    /// Native separators, and free of any `\\?\` prefix — so it is safe to open and safe to
    /// display. Not an identity key.
    #[schema(example = r"\\server\share\project\src\main.rs")]
    pub absolute_path: String,

    pub size: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub modified: OffsetDateTime,
    pub is_binary: bool,

    pub access_frecency_score: i32,
    pub modification_frecency_score: i32,
    /// Boost from recent commits on the current branch. Reported separately because
    /// `totalFrecencyScore` does **not** include it, so it would otherwise be invisible.
    pub git_recency_score: i32,
    /// Access plus modification only — deliberately excluding `gitRecencyScore`, matching
    /// the engine's own `total_frecency_score()`.
    pub total_frecency_score: i32,

    pub git_status: GitStatus,
}

impl FileItemDto {
    pub fn build(item: &FileItem, picker: &FilePicker, base_display: &str) -> Self {
        let relative_path = item.relative_path(picker);
        Self {
            absolute_path: join_native(base_display, &relative_path),
            file_name: item.file_name(picker),
            directory: item.dir_str(picker),
            size: item.size,
            modified: unix_to_offset(item.modified),
            is_binary: item.is_binary(),
            access_frecency_score: i32::from(item.access_frecency_score),
            modification_frecency_score: i32::from(item.modification_frecency_score),
            git_recency_score: i32::from(item.git_recency_score),
            total_frecency_score: item.total_frecency_score(),
            git_status: GitStatus::from_status(item.git_status),
            relative_path,
        }
    }
}

/// An indexed directory.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirItemDto {
    /// Forward slashes, with a trailing `/`.
    pub relative_path: String,
    pub dir_name: String,
    pub absolute_path: String,
    /// Highest access-frecency score among the files beneath it.
    pub max_access_frecency: i32,
}

impl DirItemDto {
    pub fn build(item: &DirItem, picker: &FilePicker, base_display: &str) -> Self {
        let relative_path = item.relative_path(picker);
        Self {
            absolute_path: join_native(base_display, &relative_path),
            dir_name: item.dir_name(picker),
            max_access_frecency: item.max_access_frecency(),
            relative_path,
        }
    }
}

/// The complete score breakdown: all 13 fields the engine computes.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScoreDto {
    pub total: i32,
    pub base_score: i32,
    pub filename_bonus: i32,
    pub special_filename_bonus: i32,
    pub frecency_boost: i32,
    /// Dropped by the C ABI and by the TypeScript SDK.
    pub git_status_boost: i32,
    /// Added in fff 0.11; absent from every other binding.
    pub git_recency_boost: i32,
    pub distance_penalty: i32,
    pub current_file_penalty: i32,
    pub combo_match_boost: i32,
    /// Dropped by the TypeScript SDK.
    pub path_alignment_bonus: i32,
    pub exact_match: bool,
    /// Documented as an **open** string, not a closed enum: it is a bare `&'static str` in
    /// the engine with no enum backing it, so a future value must not break a client.
    /// Known values include `fuzzy`, `fuzzy_filename`, `exact`, `prefix`, `path`.
    #[schema(example = "fuzzy_filename")]
    pub match_type: &'static str,
}

impl From<&Score> for ScoreDto {
    fn from(s: &Score) -> Self {
        Self {
            total: s.total,
            base_score: s.base_score,
            filename_bonus: s.filename_bonus,
            special_filename_bonus: s.special_filename_bonus,
            frecency_boost: s.frecency_boost,
            git_status_boost: s.git_status_boost,
            git_recency_boost: s.git_recency_boost,
            distance_penalty: s.distance_penalty,
            current_file_penalty: s.current_file_penalty,
            combo_match_boost: s.combo_match_boost,
            path_alignment_bonus: s.path_alignment_bonus,
            exact_match: s.exact_match,
            match_type: s.match_type,
        }
    }
}

/// Which kind of location a query specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum LocationType {
    /// `line` only, from `file.ts:42`.
    Line,
    /// `line` and `col`, from `file.ts:42:10`.
    Position,
    /// `line`/`col` as the start, `endLine`/`endCol` as the end.
    Range,
}

/// A `file.ts:42:10` suffix parsed out of the query. The MCP server discards this entirely.
///
/// `line` is always present; `col`, `endLine` and `endCol` depend on `type`.
//
// This one stays flat while ConstraintDto and MixedHit are proper discriminated unions,
// because it is the only union that appears as an *optional* field. utoipa renders
// `Option<T>` as `oneOf: [null, $ref]`, and nesting a discriminated union inside that loses
// the inheritance relationship Kiota needs ("Discriminator LineLocation is not inherited from
// LocationDto"). Generation still succeeded, but warning-free is worth more here than a union
// over three shapes that differ only in which trailing fields are set.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocationDto {
    #[serde(rename = "type")]
    #[schema(rename = "type")]
    pub location_type: LocationType,
    pub line: i32,
    /// Present for `position` and `range`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<i32>,
    /// Present for `range` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i32>,
    /// Present for `range` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_col: Option<i32>,
}

impl From<Location> for LocationDto {
    fn from(l: Location) -> Self {
        match l {
            Location::Line(line) => Self {
                location_type: LocationType::Line,
                line,
                col: None,
                end_line: None,
                end_col: None,
            },
            Location::Position { line, col } => Self {
                location_type: LocationType::Position,
                line,
                col: Some(col),
                end_line: None,
                end_col: None,
            },
            Location::Range { start, end } => Self {
                location_type: LocationType::Range,
                line: start.0,
                col: Some(start.1),
                end_line: Some(end.0),
                end_col: Some(end.1),
            },
        }
    }
}

/// Converts UTF-8 byte ranges into UTF-16 code-unit ranges against `text`.
pub fn utf16_ranges(text: &str, byte_ranges: &[(u32, u32)]) -> Vec<Utf16Range> {
    byte_ranges
        .iter()
        .map(|&(start, end)| Utf16Range {
            start: utf16_prefix_len(text, start as usize),
            end: utf16_prefix_len(text, end as usize),
        })
        .collect()
}

/// UTF-16 length of `text[..byte]`, tolerating an offset that is out of range or lands
/// inside a character — neither should happen, but a panic in a search handler would be a
/// poor way to find out.
fn utf16_prefix_len(text: &str, byte: usize) -> u32 {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    text[..b].encode_utf16().count() as u32
}

/// `base_display` is already free of verbatim prefixes; the relative part is `/`-canonical
/// and gets nativised.
///
/// Built here rather than via `FileItem::absolute_path`, which returns a mixed-separator
/// path on Windows (measured: `D:\devel\fff-server\spike/unc-probe/Cargo.toml`). The
/// engine's correct `write_absolute_path` is `pub(crate)`, so it cannot be reached from
/// outside the crate.
fn join_native(base_display: &str, relative: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let native = if sep == '/' {
        relative.to_owned()
    } else {
        relative.replace('/', &sep.to_string())
    };
    let base = base_display.trim_end_matches(sep);
    if native.is_empty() {
        base.to_owned()
    } else {
        format!("{base}{sep}{native}")
    }
}

fn unix_to_offset(secs: u64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs as i64).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_utf16_ranges_match_byte_ranges() {
        let text = "fn main() {}";
        let r = utf16_ranges(text, &[(3, 7)]);
        assert_eq!((r[0].start, r[0].end), (3, 7));
    }

    #[test]
    fn non_ascii_shifts_utf16_offsets() {
        // The APL source on the lab share is full of glyphs like this, so the conversion is
        // not hypothetical. "⍝ z" - the comment glyph is 3 UTF-8 bytes, 1 UTF-16 unit.
        let text = "⍝ z is the new External stage";
        let byte_idx = text.find("the").unwrap() as u32;
        assert_eq!(byte_idx, 9, "byte offset of 'the'");
        let r = utf16_ranges(text, &[(byte_idx, byte_idx + 3)]);
        assert_eq!(
            (r[0].start, r[0].end),
            (7, 10),
            "UTF-16 offset differs from the byte offset; a C# client indexing by bytes \
             would highlight the wrong characters"
        );
        // And the converted range really does select "the" in UTF-16 space.
        let units: Vec<u16> = text.encode_utf16().collect();
        let picked = String::from_utf16(&units[r[0].start as usize..r[0].end as usize]).unwrap();
        assert_eq!(picked, "the");
    }

    #[test]
    fn regression_real_engine_offsets_from_a_line_with_an_em_dash() {
        // Captured from the engine itself, grepping this repo's DESIGN.md for "contrary".
        // The engine reported match_byte_offsets (43, 51) on this line; slicing the line by
        // those BYTE offsets yields "contrary", while the correct UTF-16 indices are
        // (41, 49). A client indexing lineContent by the raw byte offsets would highlight
        // "d contra" instead - which is exactly what a mis-decoded test harness showed
        // before this was pinned down.
        let line = "*not* simplify UNC paths \u{2014} measured, and contrary to what an earlier draft of this document";
        assert_eq!(line.len(), 93, "bytes");
        assert_eq!(line.chars().count(), 91, "chars");
        assert_eq!(
            &line[43..51],
            "contrary",
            "engine byte offsets are genuine UTF-8"
        );

        let r = utf16_ranges(line, &[(43, 51)]);
        assert_eq!((r[0].start, r[0].end), (41, 49));

        let units: Vec<u16> = line.encode_utf16().collect();
        let picked = String::from_utf16(&units[r[0].start as usize..r[0].end as usize]).unwrap();
        assert_eq!(picked, "contrary");
    }

    #[test]
    fn astral_plane_counts_as_two_utf16_units() {
        let text = "a😀b";
        // 'b' starts at byte 5, UTF-16 index 3 (a=1, emoji=2 units).
        let r = utf16_ranges(text, &[(5, 6)]);
        assert_eq!((r[0].start, r[0].end), (3, 4));
    }

    #[test]
    fn out_of_range_and_mid_char_offsets_do_not_panic() {
        let text = "⍝ z";
        let r = utf16_ranges(text, &[(1, 99)]);
        assert_eq!(r[0].start, 0, "offset inside a character rounds down");
        assert_eq!(r[0].end, text.encode_utf16().count() as u32);
    }

    #[test]
    fn absolute_paths_are_native_and_unprefixed() {
        let joined = join_native(r"\\server\share\proj", "src/main.rs");
        #[cfg(windows)]
        assert_eq!(joined, r"\\server\share\proj\src\main.rs");
        #[cfg(not(windows))]
        assert_eq!(joined, "\\\\server\\share\\proj/src/main.rs");
    }

    #[test]
    fn clean_files_report_clean_not_null() {
        // format_git_status_opt(None) is "clean", which is the vocabulary the other
        // bindings use; the flags array is empty because no bits are set.
        let g = GitStatus::from_status(None);
        assert_eq!(g.status, "clean");
        assert!(g.flags.is_empty());
    }

    #[test]
    fn combined_status_keeps_every_bit() {
        // The case a single label cannot express, and which every other binding loses.
        let both = Status::INDEX_MODIFIED | Status::WT_MODIFIED;
        let g = GitStatus::from_status(Some(both));
        assert_eq!(g.flags, vec!["stagedModified", "modified"]);
    }
}
