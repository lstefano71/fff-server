//! Single-instance guard.
//!
//! Not optional: two servers sharing `workspaces.db_root` would open the same LMDB
//! environments for the same root and collide with `DbInUse` / `EnvSpecMismatch`. Better to
//! refuse at startup with an explanation than to fail later on a request.
//!
//! The lock is an exclusively-opened file in `db_root`, held for the process lifetime. Unlike
//! a create-if-absent marker it cannot go stale: Windows releases the handle when the process
//! dies, however it dies.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct InstanceGuard {
    _file: File,
    path: PathBuf,
}

impl InstanceGuard {
    pub fn acquire(db_root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(db_root)
            .map_err(|e| format!("cannot create {}: {e}", db_root.display()))?;
        let path = db_root.join("fff-server.lock");

        let mut file = open_exclusive(&path).map_err(|e| {
            format!(
                "another fff-server instance is already using {} ({e}).\n\
                 Stop it, or point this one at a different workspaces.db_root.",
                db_root.display()
            )
        })?;

        // Best-effort breadcrumb for whoever finds the lock and wonders who holds it.
        let _ = writeln!(file, "pid {}", std::process::id());
        let _ = file.flush();

        Ok(Self { _file: file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(windows)]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // share_mode(0) denies all other opens for as long as the handle lives.
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    // No advisory-lock dependency pulled in for a Windows-targeted server; on other
    // platforms the guard degrades to "creates the file" and relies on operator discipline.
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn second_instance_is_refused_then_allowed_after_release() {
        let dir = std::env::temp_dir().join(format!("fff-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let first = InstanceGuard::acquire(&dir).expect("first guard acquires");
        assert!(first.path().exists());

        let second = InstanceGuard::acquire(&dir);
        assert!(second.is_err(), "a second instance must be refused");
        assert!(second.unwrap_err().contains("already using"));

        drop(first);
        InstanceGuard::acquire(&dir).expect("guard is reusable once released");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
