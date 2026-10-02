//! Periodic background jobs, run inside the web process.
//!
//! Every app instance runs this loop. Jobs that must happen exactly once
//! (finalising vote rounds) take a Postgres advisory lock, so scaling to N
//! instances does not mean doing the work N times. No separate cron server,
//! no Redis — for a small app, the database you already have is enough.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;

use crate::votes;

const INTERVAL: Duration = Duration::from_secs(60);

pub fn spawn(db: PgPool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INTERVAL);
        // If a run takes longer than the interval, don't fire a burst of
        // catch-up runs afterwards.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(e) = run_once(&db).await {
                tracing::warn!(error = ?e, "background jobs failed; will retry next tick");
            }
        }
    })
}

pub async fn run_once(db: &PgPool) -> Result<(), sqlx::Error> {
    let finalized = votes::finalize_ended_rounds(db).await?;
    if finalized > 0 {
        tracing::info!(rounds = finalized, "finalised ended vote rounds");
    }
    let expired = sqlx::query("DELETE FROM sessions WHERE expires_at < now()").execute(db).await?.rows_affected();
    if expired > 0 {
        tracing::info!(sessions = expired, "deleted expired sessions");
    }
    Ok(())
}
