//! `idea-db`: the HTTP service. Neo4j is the only source of truth; there is no
//! in-memory fallback, so the process stays unready until the database answers.

use std::sync::Arc;
use std::time::Duration;

use idea_db::config::Config;
use idea_db::http::{self, AppState};
use idea_db::neo4j::Neo4j;
use idea_db::store;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env();
    let neo4j = Neo4j::new(&config).map_err(|e| e.message.clone())?;

    // Try once up front so a misconfigured deployment says so immediately, then
    // keep retrying in the background. Readiness is decided by a live query on
    // every request, not by whether this attempt succeeded.
    match store::bootstrap(&neo4j).await {
        Ok(()) => eprintln!(
            "idea-db: neo4j schema ready at {}/{}",
            config.neo4j_uri, config.neo4j_database
        ),
        Err(e) => {
            eprintln!(
                "idea-db: neo4j not ready yet ({}); retrying in background",
                e.message
            );
            let retry = neo4j.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    match store::bootstrap(&retry).await {
                        Ok(()) => {
                            eprintln!("idea-db: neo4j schema ready");
                            return;
                        }
                        Err(e) => eprintln!("idea-db: neo4j still unavailable: {}", e.message),
                    }
                }
            });
        }
    }

    let bind = config.bind.clone();
    let static_dir = config.static_dir.clone();
    let state = Arc::new(AppState { config, neo4j });
    let app = http::router(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("idea-db: listening on {bind}, serving static files from {static_dir}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    eprintln!("idea-db: stopped");
    Ok(())
}

/// Containers stop with SIGTERM; a terminal sends SIGINT. Both drain in flight
/// requests instead of cutting them off mid-transaction.
async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("idea-db: shutdown signal received");
}
