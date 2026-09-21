use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::Logging;

/// Installs this server's own subscriber.
///
/// Deliberately does **not** call `fff_search::log::init_tracing`: fff-core emits through the
/// `tracing` facade, so its internal events land here anyway, and calling its initialiser
/// would additionally install a global one-shot subscriber, a panic hook, and (on unix) a
/// SIGSEGV handler that this server has no use for.
pub fn init(cfg: &Logging) {
    // RUST_LOG wins when set, so an operator can raise fff-core's level without editing config.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(filter);
    if cfg.json {
        registry
            .with(tracing_subscriber::fmt::layer().json().with_target(true))
            .init();
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .init();
    }
}
