//! The topic vote game.
//!
//! An admin opens a *round* (e.g. 3 days) for a few tags. Each tag gets a
//! *poll*. Anyone can suggest an article for a poll and vote on suggestions,
//! without an account. Limits are per network (hashed IP):
//! `max_suggestions_per_poll` suggestions and `max_votes_per_poll` votes.
//!
//! Limits like "at most 3 votes" are classic race conditions: two requests
//! that arrive together can both read "2 votes used" and both insert. We
//! serialise requests from the same IP for the same poll with a Postgres
//! transaction-scoped advisory lock, so the check-then-insert is atomic.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{config::Config, util::humanize_duration};

/// Advisory lock key for the "finalise ended rounds" background job.
const FINALIZE_LOCK_KEY: i64 = 0x766f_7465; // "vote"

#[derive(Debug, thiserror::Error)]
pub enum VoteError {
    #[error("That poll or suggestion does not exist.")]
    NotFound,
    #[error("This round is closed.")]
    Closed,
    #[error("You have used all {0} of your votes in this poll. Remove one to vote for something else.")]
    VoteLimit(i64),
    #[error("Your network has already made {0} suggestions in this poll.")]
    SuggestionLimit(i64),
    #[error("Someone already suggested that. Vote for it instead!")]
    Duplicate,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Round {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub tag_names: Vec<String>,
    pub suggestion_count: i64,
}

impl Round {
    pub fn is_open(&self) -> bool {
        let now = Utc::now();
        self.starts_at <= now && now < self.ends_at
    }

    pub fn time_label(&self) -> String {
        let now = Utc::now();
        if self.is_open() {
            format!("ends in {}", humanize_duration((self.ends_at - now).num_seconds()))
        } else if now < self.starts_at {
            format!("starts in {}", humanize_duration((self.starts_at - now).num_seconds()))
        } else {
            format!("ended {} ago", humanize_duration((now - self.ends_at).num_seconds()))
        }
    }
}

const ROUND_SELECT: &str = "SELECT r.id, r.title, r.description, r.starts_at, r.ends_at, r.closed_at,
        COALESCE(array_agg(t.name ORDER BY t.name) FILTER (WHERE t.name IS NOT NULL), '{}') AS tag_names,
        (SELECT count(*) FROM suggestions s JOIN vote_polls p2 ON p2.id = s.poll_id
          WHERE p2.round_id = r.id AND NOT s.is_hidden) AS suggestion_count
    FROM vote_rounds r
    LEFT JOIN vote_polls p ON p.round_id = r.id
    LEFT JOIN tags t ON t.slug = p.tag_slug";

pub async fn open_rounds(db: &PgPool) -> Result<Vec<Round>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{ROUND_SELECT} WHERE r.starts_at <= now() AND r.ends_at > now() GROUP BY r.id ORDER BY r.ends_at"
    )))
    .fetch_all(db)
    .await
}

pub async fn past_rounds(db: &PgPool, limit: i64) -> Result<Vec<Round>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{ROUND_SELECT} WHERE r.ends_at <= now() GROUP BY r.id ORDER BY r.ends_at DESC LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn all_rounds(db: &PgPool) -> Result<Vec<Round>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROUND_SELECT} GROUP BY r.id ORDER BY r.ends_at DESC LIMIT 100")))
        .fetch_all(db)
        .await
}

pub async fn get_round(db: &PgPool, id: i64) -> Result<Option<Round>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ROUND_SELECT} WHERE r.id = $1 GROUP BY r.id")))
        .bind(id)
        .fetch_optional(db)
        .await
}

// --- Polls & suggestions -----------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Poll {
    pub id: i64,
    pub round_id: i64,
    pub tag_slug: String,
    pub tag_name: String,
    pub winner_suggestion_id: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Suggestion {
    pub id: i64,
    pub poll_id: i64,
    pub title: String,
    pub details: String,
    pub vote_count: i32,
    pub is_hidden: bool,
    pub voted: bool,
    pub fulfilled_slug: Option<String>,
    pub fulfilled_title: Option<String>,
    /// Winner (closed round) or current leader (open round) of its poll.
    #[sqlx(skip)]
    pub is_leader: bool,
}

/// Everything needed to render one poll for one visitor.
#[derive(Debug, Clone)]
pub struct PollView {
    pub poll: Poll,
    pub open: bool,
    pub suggestions: Vec<Suggestion>,
    pub votes_used: i64,
    pub suggestions_used: i64,
    pub max_votes: i64,
    pub max_suggestions: i64,
    /// Frozen winner once the round is finalised, otherwise the current leader.
    pub leader_id: Option<i64>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

impl PollView {
    pub fn votes_left(&self) -> i64 {
        (self.max_votes - self.votes_used).max(0)
    }

    pub fn can_suggest(&self) -> bool {
        self.open && self.suggestions_used < self.max_suggestions
    }
}

const POLL_SELECT: &str = "SELECT p.id, p.round_id, p.tag_slug, t.name AS tag_name, p.winner_suggestion_id
    FROM vote_polls p JOIN tags t ON t.slug = p.tag_slug";

async fn load_suggestions(
    db: &PgPool,
    poll_id: i64,
    ip_hash: &[u8],
    include_hidden: bool,
) -> Result<Vec<Suggestion>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.id, s.poll_id, s.title, s.details, s.vote_count, s.is_hidden,
                EXISTS (SELECT 1 FROM suggestion_votes v
                        WHERE v.suggestion_id = s.id AND v.voter_ip_hash = $2) AS voted,
                fp.slug AS fulfilled_slug, fp.title AS fulfilled_title
         FROM suggestions s
         LEFT JOIN posts fp ON fp.id = s.fulfilled_post_id AND fp.is_published
         WHERE s.poll_id = $1 AND ($3 OR NOT s.is_hidden)
         ORDER BY s.vote_count DESC, s.created_at ASC
         LIMIT 200",
    )
    .bind(poll_id)
    .bind(ip_hash)
    .bind(include_hidden)
    .fetch_all(db)
    .await
}

async fn build_view(
    db: &PgPool,
    cfg: &Config,
    poll: Poll,
    open: bool,
    ip_hash: &[u8],
    include_hidden: bool,
) -> Result<PollView, sqlx::Error> {
    let mut suggestions = load_suggestions(db, poll.id, ip_hash, include_hidden).await?;
    let (votes_used, suggestions_used): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM suggestion_votes WHERE poll_id = $1 AND voter_ip_hash = $2),
                (SELECT count(*) FROM suggestions WHERE poll_id = $1 AND author_ip_hash = $2)",
    )
    .bind(poll.id)
    .bind(ip_hash)
    .fetch_one(db)
    .await?;
    let leader_id = poll
        .winner_suggestion_id
        .or_else(|| suggestions.iter().find(|s| !s.is_hidden && s.vote_count > 0).map(|s| s.id));
    for s in &mut suggestions {
        s.is_leader = Some(s.id) == leader_id;
    }
    Ok(PollView {
        poll,
        open,
        suggestions,
        votes_used,
        suggestions_used,
        max_votes: cfg.max_votes_per_poll,
        max_suggestions: cfg.max_suggestions_per_poll,
        leader_id,
        error: None,
        notice: None,
    })
}

pub async fn polls_for_round(
    db: &PgPool,
    cfg: &Config,
    round: &Round,
    ip_hash: &[u8],
    include_hidden: bool,
) -> Result<Vec<PollView>, sqlx::Error> {
    let polls: Vec<Poll> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{POLL_SELECT} WHERE p.round_id = $1 ORDER BY t.name")))
            .bind(round.id)
            .fetch_all(db)
            .await?;
    let mut views = Vec::with_capacity(polls.len());
    for poll in polls {
        views.push(build_view(db, cfg, poll, round.is_open(), ip_hash, include_hidden).await?);
    }
    Ok(views)
}

pub async fn poll_view(
    db: &PgPool,
    cfg: &Config,
    poll_id: i64,
    ip_hash: &[u8],
) -> Result<Option<PollView>, sqlx::Error> {
    let row: Option<(i64, String, String, Option<i64>, i64, bool)> = sqlx::query_as(
        "SELECT p.round_id, p.tag_slug, t.name, p.winner_suggestion_id, p.id,
                (r.starts_at <= now() AND r.ends_at > now()) AS open
         FROM vote_polls p JOIN tags t ON t.slug = p.tag_slug JOIN vote_rounds r ON r.id = p.round_id
         WHERE p.id = $1",
    )
    .bind(poll_id)
    .fetch_optional(db)
    .await?;
    let Some((round_id, tag_slug, tag_name, winner_suggestion_id, id, open)) = row else {
        return Ok(None);
    };
    let poll = Poll { id, round_id, tag_slug, tag_name, winner_suggestion_id };
    Ok(Some(build_view(db, cfg, poll, open, ip_hash, false).await?))
}

/// Serialises concurrent requests from one network for one poll.
async fn lock_voter(tx: &mut Transaction<'_, Postgres>, poll_id: i64, ip_hash: &[u8]) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text || ':' || encode($2, 'hex'), 0))")
        .bind(poll_id)
        .bind(ip_hash)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn poll_is_open(tx: &mut Transaction<'_, Postgres>, poll_id: i64) -> Result<bool, VoteError> {
    let open: Option<bool> = sqlx::query_scalar(
        "SELECT (r.starts_at <= now() AND r.ends_at > now())
         FROM vote_polls p JOIN vote_rounds r ON r.id = p.round_id WHERE p.id = $1",
    )
    .bind(poll_id)
    .fetch_optional(&mut **tx)
    .await?;
    open.ok_or(VoteError::NotFound)
}

pub async fn submit_suggestion(
    db: &PgPool,
    cfg: &Config,
    poll_id: i64,
    ip_hash: &[u8],
    user_id: Option<i64>,
    title: &str,
    details: &str,
) -> Result<(), VoteError> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let details = details.trim();
    let len = title.chars().count();
    if !(5..=120).contains(&len) {
        return Err(VoteError::Invalid("The topic title should be 5–120 characters.".into()));
    }
    if details.chars().count() > 1000 {
        return Err(VoteError::Invalid("Details should be at most 1000 characters.".into()));
    }

    let mut tx = db.begin().await?;
    if !poll_is_open(&mut tx, poll_id).await? {
        return Err(VoteError::Closed);
    }
    lock_voter(&mut tx, poll_id, ip_hash).await?;

    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM suggestions WHERE poll_id = $1 AND author_ip_hash = $2")
        .bind(poll_id)
        .bind(ip_hash)
        .fetch_one(&mut *tx)
        .await?;
    if used >= cfg.max_suggestions_per_poll {
        return Err(VoteError::SuggestionLimit(cfg.max_suggestions_per_poll));
    }

    let inserted = sqlx::query(
        "INSERT INTO suggestions (poll_id, title, details, author_ip_hash, author_user_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (poll_id, lower(title)) DO NOTHING",
    )
    .bind(poll_id)
    .bind(&title)
    .bind(details)
    .bind(ip_hash)
    .bind(user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(VoteError::Duplicate);
    }
    tx.commit().await?;
    Ok(())
}

/// Sets this network's vote on a suggestion to `on`. Returns the poll id.
/// Like post reactions, the request carries the desired state, so retries
/// and double clicks are harmless.
pub async fn set_vote(
    db: &PgPool,
    cfg: &Config,
    suggestion_id: i64,
    ip_hash: &[u8],
    on: bool,
) -> Result<i64, VoteError> {
    let mut tx = db.begin().await?;
    let row: Option<(i64, bool)> = sqlx::query_as("SELECT poll_id, is_hidden FROM suggestions WHERE id = $1")
        .bind(suggestion_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (poll_id, hidden) = row.ok_or(VoteError::NotFound)?;
    if hidden {
        return Err(VoteError::NotFound);
    }
    if !poll_is_open(&mut tx, poll_id).await? {
        return Err(VoteError::Closed);
    }
    lock_voter(&mut tx, poll_id, ip_hash).await?;

    if on {
        let (used, already): (i64, bool) = sqlx::query_as(
            "SELECT count(*), coalesce(bool_or(suggestion_id = $3), false)
             FROM suggestion_votes WHERE poll_id = $1 AND voter_ip_hash = $2",
        )
        .bind(poll_id)
        .bind(ip_hash)
        .bind(suggestion_id)
        .fetch_one(&mut *tx)
        .await?;
        if already {
            return Ok(poll_id);
        }
        if used >= cfg.max_votes_per_poll {
            return Err(VoteError::VoteLimit(cfg.max_votes_per_poll));
        }
        sqlx::query("INSERT INTO suggestion_votes (suggestion_id, poll_id, voter_ip_hash) VALUES ($1, $2, $3)")
            .bind(suggestion_id)
            .bind(poll_id)
            .bind(ip_hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE suggestions SET vote_count = vote_count + 1 WHERE id = $1")
            .bind(suggestion_id)
            .execute(&mut *tx)
            .await?;
    } else {
        let removed = sqlx::query("DELETE FROM suggestion_votes WHERE suggestion_id = $1 AND voter_ip_hash = $2")
            .bind(suggestion_id)
            .bind(ip_hash)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if removed == 1 {
            sqlx::query("UPDATE suggestions SET vote_count = vote_count - 1 WHERE id = $1")
                .bind(suggestion_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(poll_id)
}

// --- Results -----------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Winner {
    pub round_id: i64,
    pub tag_name: String,
    pub title: Option<String>,
    pub vote_count: Option<i32>,
    pub post_slug: Option<String>,
    pub post_title: Option<String>,
}

pub async fn winners_for(db: &PgPool, round_ids: &[i64]) -> Result<HashMap<i64, Vec<Winner>>, sqlx::Error> {
    let rows: Vec<Winner> = sqlx::query_as(
        "SELECT p.round_id, t.name AS tag_name, s.title, s.vote_count, fp.slug AS post_slug, fp.title AS post_title
         FROM vote_polls p
         JOIN tags t ON t.slug = p.tag_slug
         LEFT JOIN suggestions s ON s.id = p.winner_suggestion_id
         LEFT JOIN posts fp ON fp.id = s.fulfilled_post_id AND fp.is_published
         WHERE p.round_id = ANY($1)
         ORDER BY t.name",
    )
    .bind(round_ids)
    .fetch_all(db)
    .await?;
    let mut map: HashMap<i64, Vec<Winner>> = HashMap::new();
    for w in rows {
        map.entry(w.round_id).or_default().push(w);
    }
    Ok(map)
}

/// Background job: record the winner of every poll in rounds that have ended.
/// Uses a *try* lock so that, with several app instances, exactly one does the
/// work and the others skip instead of queueing up.
pub async fn finalize_ended_rounds(db: &PgPool) -> Result<usize, sqlx::Error> {
    let mut tx = db.begin().await?;
    let got_lock: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)").bind(FINALIZE_LOCK_KEY).fetch_one(&mut *tx).await?;
    if !got_lock {
        return Ok(0);
    }
    let ended: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM vote_rounds WHERE closed_at IS NULL AND ends_at <= now() FOR UPDATE")
            .fetch_all(&mut *tx)
            .await?;
    if ended.is_empty() {
        return Ok(0);
    }
    sqlx::query(
        "UPDATE vote_polls vp SET winner_suggestion_id = (
             SELECT s.id FROM suggestions s
             WHERE s.poll_id = vp.id AND NOT s.is_hidden AND s.vote_count > 0
             ORDER BY s.vote_count DESC, s.created_at ASC
             LIMIT 1)
         WHERE vp.round_id = ANY($1)",
    )
    .bind(&ended)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE vote_rounds SET closed_at = now() WHERE id = ANY($1)").bind(&ended).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ended.len())
}

// --- Admin -------------------------------------------------------------------

pub async fn create_round(
    db: &PgPool,
    title: &str,
    description: &str,
    tag_slugs: &[String],
    duration: Duration,
    created_by: i64,
) -> Result<i64, VoteError> {
    if tag_slugs.is_empty() || tag_slugs.len() > 6 {
        return Err(VoteError::Invalid("Pick between 1 and 6 tags.".into()));
    }
    if duration < Duration::hours(1) || duration > Duration::days(31) {
        return Err(VoteError::Invalid("Duration must be between 1 hour and 31 days.".into()));
    }
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(VoteError::Invalid("Title must be 1–120 characters.".into()));
    }
    let mut tx = db.begin().await?;
    let known: i64 = sqlx::query_scalar("SELECT count(*) FROM tags WHERE slug = ANY($1)")
        .bind(tag_slugs)
        .fetch_one(&mut *tx)
        .await?;
    if known != tag_slugs.len() as i64 {
        return Err(VoteError::Invalid("Unknown tag selected.".into()));
    }
    let round_id: i64 = sqlx::query_scalar(
        "INSERT INTO vote_rounds (title, description, starts_at, ends_at, created_by)
         VALUES ($1, $2, now(), now() + $3::bigint * interval '1 second', $4) RETURNING id",
    )
    .bind(title)
    .bind(description.trim())
    .bind(duration.num_seconds())
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO vote_polls (round_id, tag_slug) SELECT $1, unnest($2::text[])")
        .bind(round_id)
        .bind(tag_slugs)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(round_id)
}

pub async fn close_round_now(db: &PgPool, round_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        // Keep the CHECK (ends_at > starts_at) satisfied even for a round
        // closed in the same second it was opened.
        "UPDATE vote_rounds SET ends_at = now(), starts_at = least(starts_at, now() - interval '1 second')
         WHERE id = $1 AND ends_at > now()",
    )
    .bind(round_id)
    .execute(db)
    .await?;
    finalize_ended_rounds(db).await?;
    Ok(())
}

pub async fn set_hidden(db: &PgPool, suggestion_id: i64, hidden: bool) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE suggestions s SET is_hidden = $2 FROM vote_polls p
         WHERE s.id = $1 AND p.id = s.poll_id RETURNING p.round_id",
    )
    .bind(suggestion_id)
    .bind(hidden)
    .fetch_optional(db)
    .await
}

/// Links a suggestion to the article that answered it (or unlinks with `None`).
pub async fn set_fulfilled(db: &PgPool, suggestion_id: i64, post_slug: Option<&str>) -> Result<i64, VoteError> {
    let post_id: Option<i64> = match post_slug {
        Some(slug) => Some(
            sqlx::query_scalar("SELECT id FROM posts WHERE slug = $1")
                .bind(slug)
                .fetch_optional(db)
                .await?
                .ok_or_else(|| VoteError::Invalid(format!("No post with slug {slug:?}.")))?,
        ),
        None => None,
    };
    sqlx::query_scalar(
        "UPDATE suggestions s SET fulfilled_post_id = $2 FROM vote_polls p
         WHERE s.id = $1 AND p.id = s.poll_id RETURNING p.round_id",
    )
    .bind(suggestion_id)
    .bind(post_id)
    .fetch_optional(db)
    .await?
    .ok_or(VoteError::NotFound)
}
