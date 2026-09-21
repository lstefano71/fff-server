//! Root canonicalisation and identity.
//!
//! The lab measurements behind this module (see DESIGN.md, Verified):
//!
//! - `dunce::canonicalize` does **not** strip `\\?\` from UNC paths, so every share-backed
//!   root keeps a verbatim prefix.
//! - The same directory reached via UNC and via a mapped drive canonicalises to two
//!   different strings — short hostname vs FQDN, and differing share casing — so a path
//!   string cannot serve as identity.
//! - A mapped drive resolves to UNC unaided, so no drive-mapping code is needed.

use std::path::{Path, PathBuf};

use crate::error::ApiError;

/// fff caps its path buffer at 4096 bytes on Windows, and `write_absolute_path` panics past
/// it. Roots are rejected early with headroom for the longest relative path beneath them.
#[cfg(windows)]
const MAX_PATH_BYTES: usize = 4096;
#[cfg(not(windows))]
const MAX_PATH_BYTES: usize = 4096;

/// Reserved so a deep relative path under an accepted root cannot reach the cap.
const RELATIVE_HEADROOM: usize = 512;

const VERBATIM_UNC: &str = r"\\?\UNC\";
const VERBATIM: &str = r"\\?\";

/// A validated root, ready to hand to `FilePicker`.
#[derive(Debug, Clone)]
pub struct CanonicalRoot {
    /// Exactly what `FilePicker` is given and what it stores as `base_path`. Keeps the
    /// verbatim prefix on shares.
    pub canonical: PathBuf,
    /// Client-facing form, verbatim prefix stripped. Display-and-open safe; never an
    /// identity key.
    pub display: String,
    /// Volume serial + file ID where the filesystem supplies one, else the case-folded
    /// canonical string. Two spellings of one directory share this.
    pub identity: String,
    /// Whether the resolved root lives on a network share. Reported to clients; nothing is
    /// special-cased on it, since freshness and eviction key off measured cost instead.
    pub is_network: bool,
}

/// Canonicalises, validates, and identifies a requested root.
///
/// Canonicalisation happens here rather than being left to `FilePicker::new`, whose
/// `canonicalize(...).unwrap_or(path)` silently keeps the raw spelling on failure and
/// degrades every downstream prefix-stripping invariant.
pub fn canonicalize_root(input: &str) -> Result<CanonicalRoot, ApiError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidPath("root must not be empty".into()));
    }

    let raw = Path::new(trimmed);
    let canonical = crate::paths::canonicalize(raw)
        .map_err(|e| ApiError::InvalidPath(format!("cannot resolve root {trimmed:?}: {e}")))?;

    let meta = std::fs::metadata(&canonical)
        .map_err(|e| ApiError::InvalidPath(format!("cannot stat root {trimmed:?}: {e}")))?;
    if !meta.is_dir() {
        return Err(ApiError::InvalidPath(format!(
            "root is not a directory: {trimmed:?}"
        )));
    }

    let canonical_str = canonical.to_string_lossy().into_owned();
    let budget = MAX_PATH_BYTES.saturating_sub(RELATIVE_HEADROOM);
    if canonical_str.len() > budget {
        return Err(ApiError::InvalidPath(format!(
            "root path is {} bytes; the engine's path buffer is {MAX_PATH_BYTES} bytes and \
             needs headroom for paths beneath it (limit {budget})",
            canonical_str.len()
        )));
    }

    // A bare share root, or a filesystem root, has no parent. fff refuses both unless
    // explicitly enabled, so reject them here with a message that explains why.
    if canonical.parent().is_none() {
        return Err(ApiError::Forbidden(format!(
            "refusing to index a filesystem or share root: {}",
            present(&canonical)
        )));
    }

    Ok(CanonicalRoot {
        display: present(&canonical),
        identity: identity_of(&canonical, &canonical_str),
        is_network: is_network_path(&canonical_str),
        canonical,
    })
}

/// Windows uses dunce so a local path does not acquire a `\\?\` prefix; UNC paths keep one
/// regardless, which is dunce's documented behaviour for forms it cannot safely simplify.
pub fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    {
        dunce::canonicalize(path)
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(path)
    }
}

/// Strips a verbatim prefix for presentation: `\\?\UNC\srv\share\x` becomes
/// `\\srv\share\x`, and `\\?\D:\x` becomes `D:\x`.
///
/// `\\?\`-prefixed strings are valid for Win32 I/O but break plenty of .NET and UI code
/// that does not expect them, and this API exists so the client need not post-process.
pub fn present(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(VERBATIM_UNC) {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(VERBATIM) {
        rest.to_owned()
    } else {
        s.into_owned()
    }
}

/// Identity is the directory's volume serial + file ID. Falls back to the case-folded
/// canonical string where the filesystem offers no usable ID, which degrades to string
/// identity rather than failing.
fn identity_of(canonical: &Path, canonical_str: &str) -> String {
    match file_id::get_file_id(canonical) {
        Ok(id) => format!("fid:{}", format_file_id(&id)),
        Err(e) => {
            tracing::debug!(
                path = %canonical.display(),
                error = %e,
                "no filesystem file id; falling back to case-folded path identity"
            );
            format!("path:{}", canonical_str.to_lowercase())
        }
    }
}

fn format_file_id(id: &file_id::FileId) -> String {
    // Formatted by hand rather than via Debug so the key does not change shape if the
    // crate's Debug output does.
    match id {
        file_id::FileId::Inode {
            device_id,
            inode_number,
        } => format!("inode/{device_id:x}/{inode_number:x}"),
        file_id::FileId::LowRes {
            volume_serial_number,
            file_index,
        } => format!("lowres/{volume_serial_number:x}/{file_index:x}"),
        file_id::FileId::HighRes {
            volume_serial_number,
            file_id,
        } => format!("highres/{volume_serial_number:x}/{file_id:x}"),
    }
}

/// True for UNC paths, verbatim or not. Mapped drives resolve to UNC during
/// canonicalisation, so they are covered without querying drive types.
fn is_network_path(canonical_str: &str) -> bool {
    canonical_str.starts_with(VERBATIM_UNC) || starts_with_plain_unc(canonical_str)
}

fn starts_with_plain_unc(s: &str) -> bool {
    s.starts_with(r"\\") && !s.starts_with(VERBATIM)
}

/// Stable 64-bit FNV-1a. Hand-rolled so a workspace id is reproducible across restarts and
/// across Rust releases, which `DefaultHasher` does not promise.
pub fn stable_id(identity: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in identity.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_verbatim_unc_for_display() {
        assert_eq!(
            present(Path::new(
                r"\\?\UNC\AZWEsofia10.scdom.net\diskK\users\Stefano\Sofia\wlm"
            )),
            r"\\AZWEsofia10.scdom.net\diskK\users\Stefano\Sofia\wlm"
        );
    }

    #[test]
    fn strips_plain_verbatim_for_display() {
        assert_eq!(present(Path::new(r"\\?\D:\devel\fff")), r"D:\devel\fff");
    }

    #[test]
    fn leaves_ordinary_paths_alone() {
        assert_eq!(present(Path::new(r"D:\devel\fff")), r"D:\devel\fff");
        assert_eq!(
            present(Path::new(r"\\server\share\proj")),
            r"\\server\share\proj"
        );
    }

    #[test]
    fn detects_network_roots_in_both_forms() {
        assert!(is_network_path(r"\\?\UNC\srv\share\x"));
        assert!(is_network_path(r"\\srv\share\x"));
        assert!(!is_network_path(r"D:\devel\fff"));
        assert!(!is_network_path(r"\\?\D:\devel\fff"));
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        // Reproducibility matters: a client re-POSTing the same root gets the same id back
        // across server restarts.
        assert_eq!(
            stable_id("fid:highres/aabb/1122"),
            stable_id("fid:highres/aabb/1122")
        );
        assert_ne!(
            stable_id("fid:highres/aabb/1122"),
            stable_id("fid:highres/aabb/1123")
        );
        assert_eq!(stable_id("x").len(), 16);
    }

    #[test]
    fn empty_root_is_rejected_before_touching_the_filesystem() {
        let err = canonicalize_root("   ").unwrap_err();
        assert_eq!(err.problem().code, "invalid-path");
    }

    #[test]
    fn missing_root_is_a_bad_request_not_a_500() {
        let err = canonicalize_root(r"Q:\definitely\not\here").unwrap_err();
        assert_eq!(err.problem().code, "invalid-path");
    }

    #[test]
    fn a_file_is_not_a_root() {
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let err = canonicalize_root(manifest).unwrap_err();
        assert!(err.problem().detail.unwrap().contains("not a directory"));
    }

    #[test]
    fn canonicalises_this_repo_and_identifies_it() {
        let root = canonicalize_root(env!("CARGO_MANIFEST_DIR")).expect("repo root resolves");
        assert!(!root.is_network, "the build directory is local");
        assert!(
            root.identity.starts_with("fid:") || root.identity.starts_with("path:"),
            "identity must be tagged: {}",
            root.identity
        );
        assert!(
            !root.display.starts_with(VERBATIM),
            "display form is unprefixed"
        );
    }

    #[test]
    fn spelling_does_not_change_identity() {
        // The whole point of file-id identity: two spellings, one workspace.
        let a = canonicalize_root(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mixed = env!("CARGO_MANIFEST_DIR").to_uppercase();
        let b = canonicalize_root(&mixed).unwrap();
        assert_eq!(a.identity, b.identity);
        assert_eq!(stable_id(&a.identity), stable_id(&b.identity));
    }

    #[test]
    fn trailing_separator_does_not_change_identity() {
        let a = canonicalize_root(env!("CARGO_MANIFEST_DIR")).unwrap();
        let b = canonicalize_root(&format!("{}\\", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(a.identity, b.identity);
    }
}
