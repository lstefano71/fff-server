pub mod pool;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use fff_search::file_picker::FilePicker;
use fff_search::frecency::FrecencyTracker;
use fff_search::query_tracker::QueryTracker;
use fff_search::{
    ContentCacheBudget, FFFMode, FilePickerOptions, GitRecencyConfig, SharedFilePicker,
    SharedFrecency, SharedQueryTracker,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::paths::CanonicalRoot;

const NOT_READY: u64 = u64::MAX;

/// How far creation should block before answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum WaitFor {
    /// Files are searchable. Fuzzy grep may still see an incomplete content index.
    #[default]
    Scan,
    /// The content index is built. Required for dependable fuzzy grep, and the watcher is
    /// installed only after this completes.
    Indexing,
    /// Live updates are flowing.
    Watcher,
}

/// Per-workspace options, all defaulting from `[defaults]` in the config file.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorkspace {
    /// Path to index. Any spelling; it is canonicalised and deduplicated by filesystem
    /// identity, so a UNC path and a mapped drive pointing at one directory give one
    /// workspace.
    pub root: String,

    /// `FFFMode::Ai` when true. Changes frecency decay, modification-score thresholds and
    /// watcher event handling. Fixed for the workspace's life.
    pub ai_mode: Option<bool>,
    /// Builds the bigram content index (~360 bytes/file). Required for typo-resistant and
    /// fuzzy grep. Measured at roughly 20x the cost of the directory walk.
    pub content_indexing: Option<bool>,
    /// Background filesystem watcher. Verified working over SMB (~500ms).
    pub watch: Option<bool>,
    pub follow_symlinks: Option<bool>,

    /// Explicit content-cache budget. Omitted, fff sizes it from the file count.
    pub cache_budget: Option<CacheBudget>,
    /// Ranking boost for files in recent commits on the current branch.
    pub git_recency: Option<GitRecency>,

    /// Overrides `workspaces.create_block_ms` for this call.
    pub wait_for_index_ms: Option<u64>,
    /// Which readiness stage to block for.
    #[serde(default)]
    pub wait_for: WaitFor,
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CacheBudget {
    #[serde(default)]
    pub max_files: usize,
    #[serde(default)]
    pub max_bytes: u64,
    #[serde(default)]
    pub max_file_size: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitRecency {
    pub enabled: Option<bool>,
    /// Commits to look back over. fff caps this at 128 internally.
    pub max_commits: Option<usize>,
    /// Commits touching more files than this are ignored, so a sweeping commit does not
    /// boost the whole tree.
    pub max_files_per_commit: Option<usize>,
}

/// The settings a workspace was actually created with, echoed back so a client never has to
/// guess which defaults applied.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveOptions {
    pub ai_mode: bool,
    pub content_indexing: bool,
    pub watch: bool,
    pub follow_symlinks: bool,
    pub git_recency_enabled: bool,
    pub git_recency_max_commits: usize,
    pub git_recency_max_files_per_commit: usize,
}

/// Readiness snapshot. `installed: false` means the background scan has not yet published
/// the picker, which is the state a client sees immediately after a 202.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub installed: bool,
    pub scanned_files_count: usize,
    pub is_scanning: bool,
    pub is_watcher_ready: bool,
    pub is_warmup_complete: bool,
    pub indexed_files: usize,
    pub has_git_repo: bool,
    pub git_root: Option<String>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A live index over one root.
pub struct Workspace {
    pub id: String,
    pub identity: String,
    pub root: CanonicalRoot,
    pub options: EffectiveOptions,
    pub db_dir: PathBuf,

    pub picker: SharedFilePicker,
    pub frecency: SharedFrecency,
    pub query_tracker: SharedQueryTracker,

    pub created_at: SystemTime,
    /// Actual time until content indexing completed. Zero while still warming.
    time_to_ready_ms: AtomicU64,

    last_touched: AtomicU64,
    /// Monotonic second at which the current index most recently became ready.
    /// `NOT_READY` while a scan or post-scan content build is active.
    ready_since: AtomicU64,
    /// Wall clock, for reporting. Kept separately so a clock change cannot disturb the
    /// rescan schedule.
    last_scan_at_unix: AtomicU64,
    origin: Instant,
}

impl Workspace {
    /// Builds the picker and blocks for the requested readiness stage, up to `budget`.
    ///
    /// Blocking is intentional: callers run this under `spawn_blocking`, since the scan of
    /// an 87k-file share took 59.5s and must never occupy a tokio worker.
    pub fn create(
        root: CanonicalRoot,
        req: &CreateWorkspace,
        config: &Config,
        budget: Duration,
    ) -> ApiResult<Self> {
        let id = crate::paths::stable_id(&root.identity);
        let db_dir = config.workspaces.db_root.join(&id);
        std::fs::create_dir_all(&db_dir).map_err(|e| {
            ApiError::Internal(format!(
                "cannot create database directory for workspace: {e}"
            ))
        })?;

        let d = &config.defaults;
        let git = req.git_recency.unwrap_or(GitRecency {
            enabled: None,
            max_commits: None,
            max_files_per_commit: None,
        });
        let git_defaults = GitRecencyConfig::default();
        let git_recency = GitRecencyConfig {
            enabled: git.enabled.unwrap_or(git_defaults.enabled),
            max_commits: git.max_commits.unwrap_or(git_defaults.max_commits),
            max_files_per_commit: git
                .max_files_per_commit
                .unwrap_or(git_defaults.max_files_per_commit),
        };

        let options = EffectiveOptions {
            ai_mode: req.ai_mode.unwrap_or(d.ai_mode),
            content_indexing: req.content_indexing.unwrap_or(d.content_indexing),
            watch: req.watch.unwrap_or(d.watch),
            follow_symlinks: req.follow_symlinks.unwrap_or(d.follow_symlinks),
            git_recency_enabled: git_recency.enabled,
            git_recency_max_commits: git_recency.max_commits,
            git_recency_max_files_per_commit: git_recency.max_files_per_commit,
        };

        let picker = SharedFilePicker::default();
        let frecency = SharedFrecency::default();
        let query_tracker = SharedQueryTracker::default();

        // Each workspace gets its own database directory: the LMDB env pool rejects two
        // trackers sharing one path with DbInUse / EnvSpecMismatch.
        frecency.init(FrecencyTracker::open(db_dir.join("frecency"))?)?;
        query_tracker.init(QueryTracker::open(db_dir.join("queries"))?)?;

        let cache_budget = req.cache_budget.and_then(|b| {
            ContentCacheBudget::from_overrides(b.max_files, b.max_bytes, b.max_file_size)
        });

        let started = Instant::now();
        FilePicker::new_with_shared_state(
            picker.clone(),
            frecency.clone(),
            FilePickerOptions {
                base_path: root.canonical.to_string_lossy().into_owned(),
                mode: if options.ai_mode {
                    FFFMode::Ai
                } else {
                    FFFMode::Neovim
                },
                enable_content_indexing: options.content_indexing,
                // Compiled out on Windows: get_cached_content returns None unconditionally
                // there, so the flag is left at its default rather than advertised.
                enable_mmap_cache: false,
                watch: options.watch,
                follow_symlinks: options.follow_symlinks,
                cache_budget,
                git_recency,
                ..Default::default()
            },
        )?;

        // Stages are sequential, not simultaneous: scan 374ms / indexing 7.76s / watcher
        // 7.76s on a 159-file share. The watcher is installed after post-scan indexing.
        picker.wait_for_scan(budget);
        let spent = started.elapsed();
        let remaining = budget.saturating_sub(spent);
        match req.wait_for {
            WaitFor::Scan => {}
            WaitFor::Indexing => {
                picker.wait_for_indexing_complete(remaining);
            }
            WaitFor::Watcher => {
                let half = remaining / 2;
                picker.wait_for_indexing_complete(half);
                picker.wait_for_watcher(remaining.saturating_sub(half));
            }
        }

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let warmup_complete = picker
            .read()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|p| p.get_scan_progress().is_warmup_complete)
            })
            .unwrap_or(false);
        let ready_at = warmup_complete.then(|| started.elapsed().as_secs());

        Ok(Self {
            id,
            identity: root.identity.clone(),
            root,
            options,
            db_dir,
            picker,
            frecency,
            query_tracker,
            created_at: SystemTime::now(),
            time_to_ready_ms: AtomicU64::new(if warmup_complete {
                elapsed_ms.max(1)
            } else {
                0
            }),
            last_touched: AtomicU64::new(0),
            ready_since: AtomicU64::new(ready_at.unwrap_or(NOT_READY)),
            last_scan_at_unix: AtomicU64::new(unix_now()),
            origin: started,
        })
    }

    /// Wall-clock time of the most recent scan, for reporting.
    pub fn last_scan_at(&self) -> u64 {
        self.last_scan_at_unix.load(Ordering::Relaxed)
    }

    /// Marks the workspace as in use, deferring idle eviction.
    pub fn touch(&self) {
        self.last_touched
            .store(self.origin.elapsed().as_secs(), Ordering::Relaxed);
    }

    pub fn idle_for(&self) -> Duration {
        let last = self.last_touched.load(Ordering::Relaxed);
        self.origin
            .elapsed()
            .saturating_sub(Duration::from_secs(last))
    }

    /// Actual initial warmup duration once known; elapsed time so far while warming.
    pub fn time_to_ready(&self) -> Duration {
        let _ = self.progress();
        let observed = self.time_to_ready_ms.load(Ordering::Acquire);
        if observed == 0 {
            self.origin.elapsed()
        } else {
            Duration::from_millis(observed)
        }
    }

    fn mark_scan_started(&self) {
        self.ready_since.store(NOT_READY, Ordering::Release);
        self.last_scan_at_unix.store(unix_now(), Ordering::Relaxed);
    }

    pub fn periodic_rescan_due(&self, interval: Duration) -> bool {
        let progress = self.progress();
        rescan_is_due(
            &progress,
            self.ready_since.load(Ordering::Acquire),
            self.origin.elapsed().as_secs(),
            interval.as_secs(),
        )
    }

    pub fn ready_for_maintenance(&self) -> bool {
        let progress = self.progress();
        !progress.is_scanning && progress.is_warmup_complete
    }

    /// Explicit rescan. The precise tool: a client that knows it changed something should
    /// say so rather than wait for the timer.
    pub fn rescan(&self) -> ApiResult<()> {
        let progress = self.progress();
        if progress.is_scanning || !progress.is_warmup_complete {
            return Err(ApiError::NotReady(
                "cannot rescan while scanning or content indexing is active".into(),
            ));
        }
        self.picker.trigger_full_rescan_async(&self.frecency)?;
        self.mark_scan_started();
        Ok(())
    }

    pub fn refresh_git_status(&self) -> ApiResult<usize> {
        Ok(self.picker.refresh_git_status(&self.frecency)?)
    }

    /// Current readiness, read straight from the engine rather than cached, so a client
    /// polling after a 202 sees the real state.
    pub fn progress(&self) -> Progress {
        let progress = match self.picker.read() {
            Ok(guard) => match guard.as_ref() {
                Some(p) => {
                    let sp = p.get_scan_progress();
                    Progress {
                        installed: true,
                        scanned_files_count: sp.scanned_files_count,
                        is_scanning: sp.is_scanning,
                        is_watcher_ready: sp.is_watcher_ready,
                        is_warmup_complete: sp.is_warmup_complete,
                        indexed_files: p.live_file_count(),
                        has_git_repo: p.has_git_repo(),
                        git_root: p.git_root().map(crate::paths::present),
                    }
                }
                // Scan thread has not committed the picker yet.
                None => Progress::default(),
            },
            Err(e) => {
                tracing::warn!(workspace = %self.id, error = %e, "picker lock unavailable");
                Progress::default()
            }
        };
        self.observe_readiness(&progress);
        progress
    }

    fn observe_readiness(&self, progress: &Progress) {
        if progress.is_scanning || !progress.is_warmup_complete {
            self.ready_since.store(NOT_READY, Ordering::Release);
            return;
        }

        let now_secs = self.origin.elapsed().as_secs();
        if self
            .ready_since
            .compare_exchange(NOT_READY, now_secs, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let elapsed_ms = self.origin.elapsed().as_millis().max(1) as u64;
            let _ = self.time_to_ready_ms.compare_exchange(
                0,
                elapsed_ms,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// Releases the index, the watcher threads and the LMDB handles.
    pub fn shutdown(&self) {
        // Safe to call from anywhere, including from inside a watch callback.
        self.picker.shutdown_watches_and_wait();
        if let Err(e) = self.frecency.destroy() {
            tracing::warn!(workspace = %self.id, error = %e, "frecency teardown failed");
        }
        if let Err(e) = self.query_tracker.destroy() {
            tracing::warn!(workspace = %self.id, error = %e, "query tracker teardown failed");
        }
    }
}

fn rescan_is_due(progress: &Progress, ready_since: u64, now_secs: u64, interval_secs: u64) -> bool {
    ready_for_maintenance(progress)
        && ready_since != NOT_READY
        && now_secs.saturating_sub(ready_since) >= interval_secs
}

fn ready_for_maintenance(progress: &Progress) -> bool {
    !progress.is_scanning && progress.is_warmup_complete
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    fn ready_progress() -> Progress {
        Progress {
            installed: true,
            is_warmup_complete: true,
            ..Progress::default()
        }
    }

    #[test]
    fn periodic_rescan_waits_for_warmup_and_a_full_ready_interval() {
        let mut progress = ready_progress();
        progress.is_warmup_complete = false;
        assert!(!ready_for_maintenance(&progress));
        assert!(!rescan_is_due(&progress, NOT_READY, 600, 60));

        progress.is_warmup_complete = true;
        progress.is_scanning = true;
        assert!(!ready_for_maintenance(&progress));
        assert!(!rescan_is_due(&progress, NOT_READY, 600, 60));

        progress.is_scanning = false;
        assert!(ready_for_maintenance(&progress));
        assert!(!rescan_is_due(&progress, 590, 600, 60));
        assert!(rescan_is_due(&progress, 540, 600, 60));
    }
}
