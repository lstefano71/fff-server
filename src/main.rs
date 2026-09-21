use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use tower_http::trace::TraceLayer;

use fff_server::config::Config;

/// Layering order is defaults, then the TOML file, then `FFF_SERVER_*`, then these flags.
#[derive(Debug, Parser)]
#[command(
    name = "fff-server",
    version,
    about = "Typed HTTP access to fff file search"
)]
struct Cli {
    /// Configuration file. Missing is fine — defaults apply.
    #[arg(
        short,
        long,
        env = "FFF_SERVER_CONFIG",
        default_value = "fff-server.toml"
    )]
    config: PathBuf,

    /// Overrides `server.bind`.
    #[arg(long)]
    bind: Option<String>,

    /// Overrides `logging.level`.
    #[arg(long)]
    log_level: Option<String>,

    /// Overrides `workspaces.db_root`.
    #[arg(long)]
    db_root: Option<PathBuf>,

    /// Print the effective configuration and exit.
    #[arg(long)]
    print_config: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let mut figment = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(&cli.config))
        .merge(Env::prefixed("FFF_SERVER_").split("__"));

    if let Some(bind) = &cli.bind {
        figment = figment.merge(("server.bind", bind.clone()));
    }
    if let Some(level) = &cli.log_level {
        figment = figment.merge(("logging.level", level.clone()));
    }
    if let Some(db_root) = &cli.db_root {
        figment = figment.merge(("workspaces.db_root", db_root.clone()));
    }

    let config: Config = figment.extract()?;

    if cli.print_config {
        println!("{}", toml_preview(&config)?);
        return Ok(());
    }

    fff_server::logging::init(&config.logging);

    let addr: SocketAddr = config.server.bind.parse().map_err(|e| {
        format!(
            "server.bind is not a socket address: {:?} ({e})",
            config.server.bind
        )
    })?;

    if config.server.token.is_none() {
        tracing::warn!(
            "no server.token configured: any caller that can reach this port can index and \
             search any path this process can read"
        );
    }

    // Held for the process lifetime: two servers over one db_root would collide on LMDB.
    // Reported as plain text and a non-zero exit rather than a Debug-quoted error, since
    // this is a startup condition an operator is meant to read and act on.
    let guard = match fff_server::guard::InstanceGuard::acquire(&config.workspaces.db_root) {
        Ok(guard) => guard,
        Err(message) => {
            eprintln!("fff-server: {message}");
            std::process::exit(1);
        }
    };
    tracing::debug!(lock = %guard.path().display(), "instance lock held");

    let (router, _api, state) = fff_server::build(config);
    let router = router.layer(TraceLayer::new_for_http());

    tokio::spawn(fff_server::maintenance(state.clone()));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        %addr,
        version = env!("CARGO_PKG_VERSION"),
        engine = fff_server::ENGINE_VERSION,
        "fff-server listening"
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;

    // Watcher threads and LMDB environments close here rather than at process exit.
    tracing::info!("draining workspaces");
    let pool = state.pool.clone();
    tokio::task::spawn_blocking(move || pool.shutdown_all()).await?;

    drop(guard);
    tracing::info!("shutdown complete");
    Ok(())
}

async fn shutdown() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("interrupt received, draining"),
        Err(e) => tracing::error!(error = %e, "failed to listen for interrupt"),
    }
}

fn toml_preview(config: &Config) -> Result<String, Box<dyn std::error::Error>> {
    // serde_json round-trip keeps this honest about what actually deserialised, without
    // pulling in a TOML serializer just for a diagnostic flag.
    Ok(serde_json::to_string_pretty(config)?)
}
