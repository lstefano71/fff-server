use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Layered configuration: struct defaults, then the TOML file, then `FFF_SERVER_*`
/// environment variables, then CLI flags. See DESIGN.md.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub workspaces: Workspaces,
    pub defaults: Defaults,
    pub logging: Logging,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Server {
    /// Lab default binds every interface. Needs an inbound Windows firewall rule.
    pub bind: String,
    /// Bearer token. `None` disables authentication entirely.
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Workspaces {
    /// How long creation may block before returning 202 and letting the client poll.
    /// A local root finishes well inside this; an 87k-file share does not.
    pub create_block_ms: u64,
    /// Parent directory for per-workspace LMDB databases. Each workspace gets its own
    /// subdirectory: the LMDB env pool rejects two trackers sharing one path.
    pub db_root: PathBuf,
    /// Empty means any readable path may be indexed.
    pub allowed_roots: Vec<PathBuf>,

    /// Rescan interval = clamp(time_to_ready * duty_factor, min, max).
    pub rescan_duty_factor: u32,
    pub rescan_min_secs: u64,
    pub rescan_max_secs: u64,

    /// Idle eviction = clamp(time_to_ready * idle_factor, min, max).
    pub idle_factor: u32,
    pub idle_min_secs: u64,
    pub idle_max_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub ai_mode: bool,
    pub content_indexing: bool,
    pub watch: bool,
    pub follow_symlinks: bool,
    pub page_size: usize,
    pub grep_page_size: usize,
    /// 0 disables the budget. A 5s budget would truncate a network grep at roughly 700 of
    /// 87k files (measured ~7ms/file over SMB), so clients page with cursors instead.
    pub grep_time_budget_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub level: String,
    pub json: bool,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".into(),
            token: None,
        }
    }
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            create_block_ms: 10_000,
            db_root: default_db_root(),
            allowed_roots: Vec::new(),
            rescan_duty_factor: 25,
            rescan_min_secs: 60,
            rescan_max_secs: 1_800,
            idle_factor: 120,
            idle_min_secs: 1_800,
            idle_max_secs: 86_400,
        }
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            ai_mode: true,
            content_indexing: true,
            watch: true,
            follow_symlinks: false,
            page_size: 100,
            grep_page_size: 50,
            grep_time_budget_ms: 0,
        }
    }
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: "info".into(),
            json: false,
        }
    }
}

fn default_db_root() -> PathBuf {
    if let Ok(dir) = std::env::var("PROGRAMDATA") {
        PathBuf::from(dir).join("fff-server").join("db")
    } else {
        std::env::temp_dir().join("fff-server").join("db")
    }
}

impl Config {
    /// Derived rescan interval for a workspace that took `time_to_ready` to become usable.
    /// Keyed off time-to-ready rather than scan duration: content indexing measured ~20x the
    /// walk, and a rescan pays both phases.
    pub fn rescan_interval(&self, time_to_ready: std::time::Duration) -> std::time::Duration {
        let scaled = time_to_ready.as_secs_f64() * f64::from(self.workspaces.rescan_duty_factor);
        clamp_secs(
            scaled,
            self.workspaces.rescan_min_secs,
            self.workspaces.rescan_max_secs,
        )
    }

    /// Derived idle timeout. Expensive indexes earn a longer life, because dropping one
    /// charges the next caller for a full rebuild.
    pub fn idle_timeout(&self, time_to_ready: std::time::Duration) -> std::time::Duration {
        let scaled = time_to_ready.as_secs_f64() * f64::from(self.workspaces.idle_factor);
        clamp_secs(
            scaled,
            self.workspaces.idle_min_secs,
            self.workspaces.idle_max_secs,
        )
    }
}

fn clamp_secs(scaled: f64, min: u64, max: u64) -> std::time::Duration {
    let secs = if scaled.is_finite() && scaled > 0.0 {
        scaled.round() as u64
    } else {
        min
    };
    std::time::Duration::from_secs(secs.clamp(min, max.max(min)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn intervals_clamp_at_both_ends() {
        let c = Config::default();

        // A trivial local tree lands on the floor rather than rescanning every second.
        assert_eq!(
            c.rescan_interval(Duration::from_millis(29)),
            Duration::from_secs(60)
        );
        // The measured 159-file UNC slice: 7.76s to ready -> 194s, inside the band.
        assert_eq!(
            c.rescan_interval(Duration::from_millis(7_760)),
            Duration::from_secs(194)
        );
        // Mid-band, to pin down the arithmetic: 60s * 25 = 1500s, still under the ceiling.
        assert_eq!(
            c.rescan_interval(Duration::from_secs(60)),
            Duration::from_secs(1_500)
        );
        // The measured 87k-file share never reached indexing-complete inside 120s, so its
        // real time_to_ready exceeds that and the ceiling applies.
        assert_eq!(
            c.rescan_interval(Duration::from_secs(120)),
            Duration::from_secs(1_800)
        );
    }

    #[test]
    fn idle_scales_with_cost() {
        let c = Config::default();
        assert_eq!(
            c.idle_timeout(Duration::from_millis(29)),
            Duration::from_secs(1_800)
        );
        // 7.76s * 120 = 931s, still under the 30min floor.
        assert_eq!(
            c.idle_timeout(Duration::from_millis(7_760)),
            Duration::from_secs(1_800)
        );
        // A minute to ready buys two hours of idle life.
        assert_eq!(
            c.idle_timeout(Duration::from_secs(60)),
            Duration::from_secs(7_200)
        );
    }

    #[test]
    fn flattening_the_band_pins_the_interval() {
        let mut c = Config::default();
        c.workspaces.rescan_min_secs = 300;
        c.workspaces.rescan_max_secs = 300;
        assert_eq!(
            c.rescan_interval(Duration::from_secs(1)),
            Duration::from_secs(300)
        );
        assert_eq!(
            c.rescan_interval(Duration::from_secs(600)),
            Duration::from_secs(300)
        );
    }
}
