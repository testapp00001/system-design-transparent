use std::sync::Arc;

use sqlx::PgPool;

use crate::{config::Config, content::roadmap::Roadmap, ratelimit::RateLimiter};

/// Shared application state. Cheap to clone: everything is behind `Arc` or is
/// already a handle (`PgPool` is internally reference counted).
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub limiter: Arc<RateLimiter>,
    pub roadmap: Arc<Roadmap>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config, roadmap: Roadmap) -> Self {
        Self { db, config: Arc::new(config), limiter: Arc::new(RateLimiter::default()), roadmap: Arc::new(roadmap) }
    }
}
