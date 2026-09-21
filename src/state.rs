use std::sync::Arc;
use std::time::Instant;

use crate::config::Config;

/// Shared handler state. The workspace pool joins this in step 2.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub started: Instant,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            started: Instant::now(),
        }
    }
}
