use std::net::SocketAddr;

use anyhow::Context;
use system_design_transparent::{
    MIGRATOR, config::Config, connect, content, router, state::AppState, templates, users, worker,
};
use tokio::signal;

const USAGE: &str = "\
Usage: system-design-transparent [COMMAND]

Commands:
  serve                     Run migrations, sync content and start the web server (default)
  sync-content              Run migrations and sync content/ into the database, then exit
  check-content             Validate content/ without touching the database (used in CI)
  make-admin <username>     Grant admin rights to an existing user
  help                      Show this message

Configuration is read from environment variables; see .env.example.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => serve().await,
        Some("sync-content") => {
            let config = Config::from_env()?;
            let db = connect(&config.database_url, 2).await?;
            MIGRATOR.run(&db).await?;
            let library = content::load_library(&config.content_dir)?;
            let report = content::sync(&db, &library).await?;
            println!("{report:?}");
            Ok(())
        }
        Some("check-content") => {
            let dir = std::env::var("CONTENT_DIR").unwrap_or_else(|_| "content".into());
            let library = content::load_library(dir.as_ref())?;
            println!(
                "content OK: {} posts, {} tags, {} roadmap sections",
                library.posts.len(),
                library.tags.len(),
                library.roadmap.sections.len()
            );
            Ok(())
        }
        Some("make-admin") => {
            let username = args.get(1).context("usage: make-admin <username>")?;
            let config = Config::from_env()?;
            let db = connect(&config.database_url, 1).await?;
            if users::set_admin(&db, username, true).await? {
                println!("{username} is now an admin");
                Ok(())
            } else {
                anyhow::bail!("no user named {username:?}; register through the website first")
            }
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let max_conns = std::env::var("DATABASE_MAX_CONNECTIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let db = connect(&config.database_url, max_conns).await.context("connecting to Postgres")?;

    // Migrations take an advisory lock, so several instances starting at once
    // are safe.
    MIGRATOR.run(&db).await.context("running migrations")?;

    let library = content::load_library(&config.content_dir)?;
    let report = content::sync(&db, &library).await?;
    tracing::info!(?report, "content synced");

    templates::init_asset_version(&config.static_dir);
    if config.run_background_jobs {
        worker::spawn(db.clone());
    }

    let addr = config.bind_addr;
    let state = AppState::new(db, config, library.roadmap);
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("shut down cleanly");
    Ok(())
}

/// Stop accepting new connections on Ctrl+C or SIGTERM (what Docker and
/// Kubernetes send), and let in-flight requests finish.
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate()).expect("install SIGTERM handler").recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
