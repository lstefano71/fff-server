use std::sync::Arc;
use std::time::Instant;

use crate::config::Config;
use crate::workspace::pool::Pool;

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: Arc<Pool>,
    pub started: Instant,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let config = Arc::new(config);
        Self {
            pool: Arc::new(Pool::new(config.clone())),
            config,
            started: Instant::now(),
        }
    }
}
