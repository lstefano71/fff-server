use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::workspace::{EffectiveOptions, Workspace};

/// Readiness, as a single field a client can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceStatus {
    /// The background scan has not published an index yet. Poll.
    Initialising,
    /// Files are searchable, but the content index is still building, so fuzzy grep may see
    /// an incomplete picture.
    Indexing,
    /// Fully built: searchable, content-indexed, and watching.
    Ready,
}

/// A workspace as clients see it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResource {
    /// Stable identifier, derived from the directory's filesystem identity. Re-creating the
    /// same root — under any spelling, and across server restarts — returns this same id.
    #[schema(example = "9a4f1c2b7e0d5613")]
    pub id: String,
    /// The resolved root, with any verbatim `\\?\` prefix stripped. Display-and-open safe;
    /// not an identity key.
    #[schema(example = r"\\server\share\project")]
    pub root: String,
    /// True when the root resolved to a UNC path, mapped drives included. Reported for
    /// visibility only: freshness and eviction key off measured cost, not path type.
    pub is_network_path: bool,

    pub status: WorkspaceStatus,
    pub is_scanning: bool,
    /// The content index is built. Fuzzy grep needs this, and the watcher is installed only
    /// after it completes.
    pub is_warmup_complete: bool,
    pub is_watcher_ready: bool,

    /// Files currently in the index.
    pub indexed_files: usize,
    /// Files seen by the most recent scan.
    pub scanned_files_count: usize,

    pub has_git_repo: bool,
    /// Git worktree root, if one was discovered.
    pub git_root: Option<String>,

    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Observed time from construction until content indexing first completed. While the
    /// workspace is still warming, this is elapsed time so far.
    pub time_to_ready_ms: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub last_scan_at: OffsetDateTime,
    /// Effective automatic rescan interval, derived from `timeToReadyMs`.
    pub rescan_interval_secs: u64,
    /// Effective idle-eviction timeout, derived from `timeToReadyMs`.
    pub idle_timeout_secs: u64,

    /// The settings this workspace was actually built with.
    pub options: EffectiveOptions,
}

impl WorkspaceResource {
    pub fn build(ws: &Workspace, rescan_interval_secs: u64, idle_timeout_secs: u64) -> Self {
        let p = ws.progress();
        let status = if !p.installed {
            WorkspaceStatus::Initialising
        } else if p.is_scanning || !p.is_warmup_complete {
            WorkspaceStatus::Indexing
        } else {
            WorkspaceStatus::Ready
        };

        Self {
            id: ws.id.clone(),
            root: ws.root.display.clone(),
            is_network_path: ws.root.is_network,
            status,
            is_scanning: p.is_scanning,
            is_warmup_complete: p.is_warmup_complete,
            is_watcher_ready: p.is_watcher_ready,
            indexed_files: p.indexed_files,
            scanned_files_count: p.scanned_files_count,
            has_git_repo: p.has_git_repo,
            git_root: p.git_root,
            created_at: OffsetDateTime::from(ws.created_at),
            time_to_ready_ms: ws.time_to_ready().as_millis() as u64,
            last_scan_at: unix_to_offset(ws.last_scan_at()),
            rescan_interval_secs,
            idle_timeout_secs,
            options: ws.options.clone(),
        }
    }
}

/// Listing response.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceList {
    pub workspaces: Vec<WorkspaceResource>,
}

fn unix_to_offset(secs: u64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs as i64).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}
