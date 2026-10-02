//! System Design Transparent — an open knowledge base about how production
//! backends are built.
//!
//! Layout of the code (a plain layered design, deliberately not more):
//!
//! - `routes/*`   HTTP handlers: parse input, call the domain, render HTML.
//! - `posts`, `votes`, `users`, `auth`   domain logic and SQL.
//! - `content`    Markdown files -> Postgres sync.
//! - `templates`  Askama templates + view models.
//! - `worker`     periodic background jobs.

pub mod auth;
pub mod config;
pub mod content;
pub mod error;
pub mod ip;
pub mod posts;
pub mod ratelimit;
pub mod routes;
pub mod security;
pub mod state;
pub mod templates;
pub mod users;
pub mod util;
pub mod votes;
pub mod worker;

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};

pub use routes::router;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Opens the connection pool. Each Postgres connection is a whole OS process
/// on the server, so the pool is deliberately small; see the article on
/// connection pooling for how to size it.
pub async fn connect(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        // Fail fast instead of piling up requests when the pool is exhausted.
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(10 * 60))
        .connect(database_url)
        .await
}
