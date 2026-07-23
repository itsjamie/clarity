#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use clarity_server::{AppConfig, AppState, build_router};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let config = AppConfig::from_env().context("invalid Clarity Share configuration")?;
    init_tracing(&config);
    let address = config.bind_address;
    let state = AppState::new(config);
    let registry = state.registry.clone();
    let listener = TcpListener::bind(address)
        .await
        .with_context(|| format!("bind clarity-server to {address}"))?;
    info!(%address, "clarity-server ready");
    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        registry.shutdown().await;
    })
    .await
    .context("serve Clarity Share")?;
    info!("clarity-server stopped");
    Ok(())
}

fn init_tracing(config: &AppConfig) {
    let filter = EnvFilter::try_new(&config.log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    if config.environment == clarity_server::config::Environment::Production {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .compact()
            .init();
    }
}

async fn shutdown_signal() {
    let control_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = control_c => {}, _ = terminate => {} }
}
