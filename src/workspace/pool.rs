use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::paths::{self, CanonicalRoot};
use crate::workspace::{CreateWorkspace, Workspace};

/// Whether a create call built a new index or handed back an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    New,
    Existing,
}

/// Keyed by the stable id derived from filesystem identity, so two spellings of one
/// directory resolve to one entry without a second lookup table.
pub struct Pool {
    entries: RwLock<HashMap<String, Arc<Workspace>>>,
    config: Arc<Config>,
}

impl Pool {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, id: &str) -> Option<Arc<Workspace>> {
        let ws = self.entries.read().get(id).cloned();
        if let Some(ws) = &ws {
            ws.touch();
        }
        ws
    }

    pub fn list(&self) -> Vec<Arc<Workspace>> {
        let mut all: Vec<_> = self.entries.read().values().cloned().collect();
        all.sort_by(|a, b| a.root.display.cmp(&b.root.display));
        all
    }

    pub fn remove(&self, id: &str) -> Option<Arc<Workspace>> {
        let removed = self.entries.write().remove(id);
        if let Some(ws) = &removed {
            ws.shutdown();
        }
        removed
    }

    /// Resolves, validates and enforces the allowlist, without building anything.
    pub fn resolve_root(&self, root: &str) -> ApiResult<CanonicalRoot> {
        let resolved = paths::canonicalize_root(root)?;
        let allowed = &self.config.workspaces.allowed_roots;
        if allowed.is_empty() {
            return Ok(resolved);
        }
        for permitted in allowed {
            if let Ok(p) = paths::canonicalize(permitted)
                && resolved.canonical.starts_with(&p)
            {
                return Ok(resolved);
            }
        }
        Err(ApiError::Forbidden(format!(
            "root {} is not under any configured allowed_roots entry",
            resolved.display
        )))
    }

    /// Existing workspace for this root, if any. Lets a handler avoid the expensive path
    /// entirely when the index is already warm.
    pub fn existing(&self, root: &CanonicalRoot) -> Option<Arc<Workspace>> {
        self.get(&paths::stable_id(&root.identity))
    }

    /// Builds a workspace and installs it. Blocking — call under `spawn_blocking`.
    ///
    /// A concurrent caller that won the race is honoured rather than overwritten, so two
    /// simultaneous creates for one root cannot leave two indexes behind.
    pub fn create_blocking(
        &self,
        root: CanonicalRoot,
        req: &CreateWorkspace,
    ) -> ApiResult<(Arc<Workspace>, Created)> {
        let id = paths::stable_id(&root.identity);
        if let Some(existing) = self.get(&id) {
            return Ok((existing, Created::Existing));
        }

        let budget = Duration::from_millis(
            req.wait_for_index_ms
                .unwrap_or(self.config.workspaces.create_block_ms),
        );

        tracing::info!(
            workspace = %id,
            root = %root.display,
            network = root.is_network,
            budget_ms = budget.as_millis(),
            "creating workspace"
        );
        let workspace = Arc::new(Workspace::create(root, req, &self.config, budget)?);

        let mut entries = self.entries.write();
        if let Some(winner) = entries.get(&id) {
            // Lost the race. Discard ours rather than leaving an orphaned index and its
            // watcher threads running.
            let winner = winner.clone();
            drop(entries);
            workspace.shutdown();
            return Ok((winner, Created::Existing));
        }
        entries.insert(id, workspace.clone());
        drop(entries);

        workspace.touch();
        tracing::info!(
            workspace = %workspace.id,
            time_to_ready_ms = workspace.time_to_ready().as_millis(),
            rescan_interval_s = self.rescan_interval(&workspace).as_secs(),
            idle_timeout_s = self.idle_timeout(&workspace).as_secs(),
            "workspace created"
        );
        Ok((workspace, Created::New))
    }

    pub fn rescan_interval(&self, ws: &Workspace) -> Duration {
        self.config.rescan_interval(ws.time_to_ready())
    }

    pub fn idle_timeout(&self, ws: &Workspace) -> Duration {
        self.config.idle_timeout(ws.time_to_ready())
    }

    /// One sweep of the maintenance loop. Returns (rescans triggered, workspaces evicted).
    ///
    /// Both intervals derive from measured time-to-ready, so a trivial tree rescans briskly
    /// while an 87k-file share does not spend a fifth of its life rescanning.
    pub fn sweep(&self) -> (usize, usize) {
        let candidates: Vec<Arc<Workspace>> = self.entries.read().values().cloned().collect();
        let mut rescanned = 0;
        let mut evicted = 0;

        for ws in candidates {
            if !ws.ready_for_maintenance() {
                continue;
            }

            if ws.idle_for() >= self.idle_timeout(&ws) {
                tracing::info!(
                    workspace = %ws.id,
                    idle_s = ws.idle_for().as_secs(),
                    "evicting idle workspace"
                );
                self.remove(&ws.id);
                evicted += 1;
                continue;
            }

            if ws.periodic_rescan_due(self.rescan_interval(&ws)) {
                match ws.rescan() {
                    Ok(()) => {
                        tracing::debug!(workspace = %ws.id, "periodic rescan triggered");
                        rescanned += 1;
                    }
                    // Throttled or not yet initialised: the next sweep retries.
                    Err(e) => tracing::debug!(workspace = %ws.id, error = %e, "rescan skipped"),
                }
            }
        }

        (rescanned, evicted)
    }

    /// Tears everything down on shutdown so watcher threads and LMDB environments close
    /// before the process exits.
    pub fn shutdown_all(&self) {
        let ids: Vec<String> = self.entries.read().keys().cloned().collect();
        for id in ids {
            self.remove(&id);
        }
    }
}
