//! Reading, searching and reacting to posts.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::{content::LEVELS, util::escape_html};

pub const PAGE_SIZE: i64 = 12;

/// Sentinels used to mark search hits in `ts_headline` output. We escape the
/// snippet ourselves and then turn these into `<mark>` tags, so article text
/// containing `<` can never inject HTML.
const HIT_START: char = '\u{1}';
const HIT_END: char = '\u{2}';

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PostCard {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub level: String,
    pub tags: Vec<String>,
    pub reading_minutes: i32,
    pub published_at: DateTime<Utc>,
    pub like_count: i32,
    pub upvote_count: i32,
    pub save_count: i32,
    /// Search snippet, already HTML-escaped with `<mark>` around hits.
    pub headline: Option<String>,
    /// Total rows matching the filter (same on every row, via a window function).
    pub total: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PostDetail {
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub level: String,
    pub tags: Vec<String>,
    pub body_html: String,
    pub toc_html: String,
    pub reading_minutes: i32,
    pub published_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub like_count: i32,
    pub upvote_count: i32,
    pub save_count: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Relevance,
    Newest,
    Trending,
    Top,
    Liked,
}

impl Sort {
    pub const CHOICES: [(&'static str, &'static str); 5] = [
        ("relevance", "Best match"),
        ("newest", "Newest"),
        ("trending", "Trending"),
        ("top", "Most upvoted"),
        ("liked", "Most liked"),
    ];

    /// The order used when the visitor didn't pick one: best match while
    /// searching, newest otherwise.
    pub fn default_for(has_query: bool) -> Self {
        if has_query { Sort::Relevance } else { Sort::Newest }
    }

    /// Returns the sort to use and whether it differs from the default. An
    /// empty or unknown value means "default", which is what the form's
    /// first option sends, so typing a search ranks by relevance.
    fn parse(s: Option<&str>, has_query: bool) -> (Self, bool) {
        let default = Sort::default_for(has_query);
        let chosen = match s.map(str::trim) {
            Some("newest") => Some(Sort::Newest),
            Some("trending") => Some(Sort::Trending),
            Some("top") => Some(Sort::Top),
            Some("liked") => Some(Sort::Liked),
            Some("relevance") if has_query => Some(Sort::Relevance),
            _ => None,
        };
        match chosen {
            Some(sort) if sort != default => (sort, true),
            _ => (default, false),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Relevance => "relevance",
            Sort::Newest => "newest",
            Sort::Trending => "trending",
            Sort::Top => "top",
            Sort::Liked => "liked",
        }
    }
}

/// Raw query-string parameters for the listing page.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ListParams {
    pub q: Option<String>,
    pub tag: Option<String>,
    pub level: Option<String>,
    pub sort: Option<String>,
    pub page: Option<i64>,
}

/// Validated, normalised filter.
#[derive(Debug, Clone)]
pub struct ListFilter {
    pub q: String,
    pub tsquery: Option<String>,
    pub tag: Option<String>,
    pub level: Option<String>,
    pub sort: Sort,
    /// True when the visitor picked a non-default sort.
    pub explicit_sort: bool,
    pub page: i64,
}

impl ListFilter {
    pub fn from_params(p: &ListParams) -> Self {
        let q: String = p.q.as_deref().unwrap_or("").trim().chars().take(200).collect();
        let tsquery = build_prefix_tsquery(&q);
        let non_empty = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        let (sort, explicit_sort) = Sort::parse(p.sort.as_deref(), tsquery.is_some());
        Self {
            sort,
            explicit_sort,
            tsquery,
            q,
            tag: non_empty(&p.tag),
            level: non_empty(&p.level).filter(|l| LEVELS.contains(&l.as_str())),
            page: p.page.unwrap_or(1).clamp(1, 1000),
        }
    }

    /// Query string for this filter on a different page.
    pub fn page_query(&self, page: i64) -> String {
        let mut parts = Vec::new();
        let enc = |s: &str| urlencode(s);
        if !self.q.is_empty() {
            parts.push(format!("q={}", enc(&self.q)));
        }
        if let Some(t) = &self.tag {
            parts.push(format!("tag={}", enc(t)));
        }
        if let Some(l) = &self.level {
            parts.push(format!("level={}", enc(l)));
        }
        if self.explicit_sort {
            parts.push(format!("sort={}", self.sort.as_str()));
        }
        if page > 1 {
            parts.push(format!("page={page}"));
        }
        parts.join("&")
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Turns free text into a prefix tsquery: `"conn pool"` -> `conn:* & pool:*`.
/// Only alphanumeric tokens survive, so user input can never produce a
/// tsquery syntax error.
pub fn build_prefix_tsquery(q: &str) -> Option<String> {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(8)
        .map(|t| format!("{}:*", t.to_lowercase()))
        .collect();
    if terms.is_empty() { None } else { Some(terms.join(" & ")) }
}

fn finish_headline(raw: &str) -> String {
    escape_html(raw).replace(HIT_START, "<mark>").replace(HIT_END, "</mark>")
}

const CARD_COLUMNS: &str = "p.slug, p.title, p.summary, p.level, p.tags, p.reading_minutes, p.published_at, \
                            p.like_count, p.upvote_count, p.save_count";

pub async fn list(db: &PgPool, f: &ListFilter) -> Result<Vec<PostCard>, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT ");
    qb.push(CARD_COLUMNS).push(", count(*) OVER () AS total, ");

    match &f.tsquery {
        Some(tsq) => {
            qb.push("ts_headline('english', p.body_text, to_tsquery('english', ")
                .push_bind(tsq.clone())
                .push("), ")
                .push_bind(format!("MaxFragments=1, MaxWords=30, MinWords=15, StartSel={HIT_START}, StopSel={HIT_END}"))
                .push(") AS headline");
        }
        None => {
            qb.push("NULL::text AS headline");
        }
    }

    qb.push(" FROM posts p");
    push_where(&mut qb, f);

    qb.push(" ORDER BY ");
    match (f.sort, &f.tsquery) {
        (Sort::Relevance, Some(tsq)) => {
            qb.push("ts_rank_cd(p.search_vector, to_tsquery('english', ")
                .push_bind(tsq.clone())
                .push(")) + word_similarity(")
                .push_bind(f.q.clone())
                .push(", p.title) DESC, ");
        }
        (Sort::Trending, _) => {
            // Hacker News style gravity: engagement decays with age. The age is
            // clamped at 0 so a post dated in the future can't make the base
            // negative (power() would raise an error) or zero.
            qb.push(
                "(p.upvote_count * 2 + p.like_count + p.save_count + 1) \
                 / power(greatest(extract(epoch FROM now() - p.published_at), 0) / 3600 + 2, 1.5) DESC, ",
            );
        }
        (Sort::Top, _) => {
            qb.push("p.upvote_count DESC, ");
        }
        (Sort::Liked, _) => {
            qb.push("p.like_count DESC, ");
        }
        _ => {}
    }
    qb.push("p.published_at DESC, p.id DESC LIMIT ")
        .push_bind(PAGE_SIZE)
        .push(" OFFSET ")
        .push_bind((f.page - 1) * PAGE_SIZE);

    let mut cards: Vec<PostCard> = qb.build_query_as().fetch_all(db).await?;
    for c in &mut cards {
        c.headline = c.headline.as_deref().map(finish_headline);
    }
    Ok(cards)
}

/// The WHERE clause shared by `list` and `count`, so they always agree.
fn push_where(qb: &mut QueryBuilder<Postgres>, f: &ListFilter) {
    qb.push(" WHERE p.is_published");
    if let Some(tag) = &f.tag {
        qb.push(" AND p.tags @> ARRAY[").push_bind(tag.clone()).push("]::text[]");
    }
    if let Some(level) = &f.level {
        qb.push(" AND p.level = ").push_bind(level.clone());
    }
    if let Some(tsq) = &f.tsquery {
        // Full-text match, OR a fuzzy title match so small typos still work.
        qb.push(" AND (p.search_vector @@ to_tsquery('english', ")
            .push_bind(tsq.clone())
            .push(") OR ")
            .push_bind(f.q.clone())
            .push(" <% p.title)");
    }
}

/// Number of posts matching the filter (ignores paging).
pub async fn count(db: &PgPool, f: &ListFilter) -> Result<i64, sqlx::Error> {
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT count(*) FROM posts p");
    push_where(&mut qb, f);
    qb.build_query_scalar().fetch_one(db).await
}

pub async fn get_by_slug(db: &PgPool, slug: &str) -> Result<Option<PostDetail>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, slug, title, summary, level, tags, body_html, toc_html, reading_minutes,
                published_at, updated_at, like_count, upvote_count, save_count
         FROM posts WHERE slug = $1 AND is_published",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
}

/// Posts sharing the most tags with the given one.
pub async fn related(db: &PgPool, post: &PostDetail) -> Result<Vec<PostCard>, sqlx::Error> {
    sqlx::query_as(
        "SELECT p.slug, p.title, p.summary, p.level, p.tags, p.reading_minutes, p.published_at,
                p.like_count, p.upvote_count, p.save_count, NULL::text AS headline, 0::bigint AS total
         FROM posts p
         WHERE p.is_published AND p.id <> $1 AND p.tags && $2
         ORDER BY cardinality(ARRAY(SELECT unnest(p.tags) INTERSECT SELECT unnest($2::text[]))) DESC,
                  p.upvote_count DESC, p.published_at DESC
         LIMIT 4",
    )
    .bind(post.id)
    .bind(&post.tags)
    .fetch_all(db)
    .await
}

// --- Reactions ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    Like,
    Upvote,
    Save,
}

impl Reaction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "like" => Some(Reaction::Like),
            "upvote" => Some(Reaction::Upvote),
            "save" => Some(Reaction::Save),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Reaction::Like => "like",
            Reaction::Upvote => "upvote",
            Reaction::Save => "save",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReactionState {
    pub liked: bool,
    pub upvoted: bool,
    pub saved: bool,
    pub like_count: i32,
    pub upvote_count: i32,
    pub save_count: i32,
}

pub async fn reaction_state(db: &PgPool, user_id: Option<i64>, post_id: i64) -> Result<ReactionState, sqlx::Error> {
    let (like_count, upvote_count, save_count): (i32, i32, i32) =
        sqlx::query_as("SELECT like_count, upvote_count, save_count FROM posts WHERE id = $1")
            .bind(post_id)
            .fetch_one(db)
            .await?;
    let mut state = ReactionState { like_count, upvote_count, save_count, ..Default::default() };
    if let Some(uid) = user_id {
        let kinds: Vec<String> =
            sqlx::query_scalar("SELECT kind FROM post_reactions WHERE user_id = $1 AND post_id = $2")
                .bind(uid)
                .bind(post_id)
                .fetch_all(db)
                .await?;
        state.liked = kinds.iter().any(|k| k == "like");
        state.upvoted = kinds.iter().any(|k| k == "upvote");
        state.saved = kinds.iter().any(|k| k == "save");
    }
    Ok(state)
}

/// Sets a reaction to the desired state. The client sends the state it
/// *wants* (on/off) rather than "toggle", which makes the request idempotent:
/// a double click or a retried request cannot flip it back.
pub async fn set_reaction(
    db: &PgPool,
    user_id: i64,
    post_id: i64,
    kind: Reaction,
    on: bool,
) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    let changed = if on {
        sqlx::query("INSERT INTO post_reactions (user_id, post_id, kind) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
    } else {
        sqlx::query("DELETE FROM post_reactions WHERE user_id = $1 AND post_id = $2 AND kind = $3")
    }
    .bind(user_id)
    .bind(post_id)
    .bind(kind.as_str())
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // Only touch the counter if a row was really inserted/deleted.
    if changed == 1 {
        sqlx::query(
            "UPDATE posts SET
                like_count   = like_count   + CASE WHEN $2 = 'like'   THEN $3 ELSE 0 END,
                upvote_count = upvote_count + CASE WHEN $2 = 'upvote' THEN $3 ELSE 0 END,
                save_count   = save_count   + CASE WHEN $2 = 'save'   THEN $3 ELSE 0 END
             WHERE id = $1",
        )
        .bind(post_id)
        .bind(kind.as_str())
        .bind(if on { 1i32 } else { -1i32 })
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn list_for_user(db: &PgPool, user_id: i64, kind: Reaction) -> Result<Vec<PostCard>, sqlx::Error> {
    sqlx::query_as(
        "SELECT p.slug, p.title, p.summary, p.level, p.tags, p.reading_minutes, p.published_at,
                p.like_count, p.upvote_count, p.save_count, NULL::text AS headline, 0::bigint AS total
         FROM post_reactions r JOIN posts p ON p.id = r.post_id
         WHERE r.user_id = $1 AND r.kind = $2 AND p.is_published
         ORDER BY r.created_at DESC
         LIMIT 200",
    )
    .bind(user_id)
    .bind(kind.as_str())
    .fetch_all(db)
    .await
}

// --- Tags --------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TagCount {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub post_count: i64,
}

pub async fn tags_with_counts(db: &PgPool) -> Result<Vec<TagCount>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.slug, t.name, t.description, count(p.id) AS post_count
         FROM tags t
         LEFT JOIN posts p ON p.is_published AND p.tags @> ARRAY[t.slug]
         GROUP BY t.slug
         ORDER BY count(p.id) DESC, t.name",
    )
    .fetch_all(db)
    .await
}

pub async fn published_slugs(db: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT slug FROM posts WHERE is_published").fetch_all(db).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsquery_is_sanitised() {
        assert_eq!(build_prefix_tsquery("conn pool"), Some("conn:* & pool:*".into()));
        assert_eq!(build_prefix_tsquery("a|b & !c:*"), Some("a:* & b:* & c:*".into()));
        assert_eq!(build_prefix_tsquery("  ()!  "), None);
    }

    #[test]
    fn headline_is_escaped_before_marking() {
        let raw = format!("use {HIT_START}Vec{HIT_END}<script>");
        assert_eq!(finish_headline(&raw), "use <mark>Vec</mark>&lt;script&gt;");
    }

    #[test]
    fn filter_defaults() {
        let f = ListFilter::from_params(&ListParams::default());
        assert_eq!(f.sort, Sort::Newest);
        assert_eq!(f.page, 1);
        let f = ListFilter::from_params(&ListParams {
            q: Some("cache".into()),
            level: Some("guru".into()),
            ..Default::default()
        });
        assert_eq!(f.sort, Sort::Relevance);
        assert_eq!(f.level, None);
        assert_eq!(f.page_query(2), "q=cache&page=2");
    }

    #[test]
    fn empty_sort_means_default() {
        // What the search form sends while its first ("default") option is selected.
        let p = |q: &str, sort: &str| ListParams { q: Some(q.into()), sort: Some(sort.into()), ..Default::default() };
        let f = ListFilter::from_params(&p("pool", ""));
        assert_eq!((f.sort, f.explicit_sort), (Sort::Relevance, false));
        let f = ListFilter::from_params(&p("", ""));
        assert_eq!((f.sort, f.explicit_sort), (Sort::Newest, false));
        // "newest" is the default without a query, but an explicit choice with one.
        let f = ListFilter::from_params(&p("", "newest"));
        assert_eq!((f.sort, f.explicit_sort), (Sort::Newest, false));
        let f = ListFilter::from_params(&p("pool", "newest"));
        assert_eq!((f.sort, f.explicit_sort), (Sort::Newest, true));
        assert_eq!(f.page_query(1), "q=pool&sort=newest");
        let f = ListFilter::from_params(&p("", "relevance"));
        assert_eq!((f.sort, f.explicit_sort), (Sort::Newest, false));
    }
}
