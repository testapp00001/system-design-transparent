//! Askama templates and the small view models they render.
//!
//! Templates are compiled into the binary and type-checked at build time, so
//! a typo in a template is a compiler error rather than a runtime 500.

use std::{path::Path, sync::OnceLock};

use askama::Template;
use axum::{
    extract::FromRequestParts,
    http::{HeaderValue, header, request::Parts},
    response::{Html, IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::{
    auth::{CurrentUser, SessionUser},
    error::AppError,
    posts::{ListFilter, PostCard, ReactionState, Sort, TagCount},
    votes::{PollView, Round, Winner},
};

// --- Request context ---------------------------------------------------------

/// Per-request information every page needs.
pub struct Ctx {
    pub user: Option<SessionUser>,
    /// Path and query of the current request, used for `?next=` after login.
    pub path: String,
    /// True for htmx requests that want a fragment instead of a full page.
    pub htmx: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        let path = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
        let header_is = |name: &str| parts.headers.get(name).is_some_and(|v| v == "true");
        // A history-restore request (back button after a cache miss) needs the
        // whole page even though htmx sent it.
        let htmx = header_is("hx-request") && !header_is("hx-history-restore-request");
        Ok(Ctx { user, path, htmx })
    }
}

impl Ctx {
    pub fn user_id(&self) -> Option<i64> {
        self.user.as_ref().map(|u| u.id)
    }
}

// --- Rendering helpers ---------------------------------------------------------

static ASSET_VERSION: OnceLock<String> = OnceLock::new();

/// Fingerprints static assets so we can cache them aggressively and still
/// bust the cache on deploy (`app.css?v=<hash>`).
pub fn init_asset_version(static_dir: &Path) {
    let mut hasher = Sha256::new();
    for file in ["css/app.css", "js/app.js", "js/theme.js", "js/htmx.min.js"] {
        if let Ok(bytes) = std::fs::read(static_dir.join(file)) {
            hasher.update(&bytes);
        }
    }
    let _ = ASSET_VERSION.set(hex::encode(hasher.finalize())[..10].to_string());
}

fn asset_version() -> &'static str {
    ASSET_VERSION.get().map(String::as_str).unwrap_or("dev")
}

pub fn render<T: Template>(template: &T) -> Result<Response, AppError> {
    let mut res = Html(template.render()?).into_response();
    // Full pages and htmx fragments share URLs, so caches must key on this header.
    res.headers_mut().insert(header::VARY, HeaderValue::from_static("HX-Request"));
    Ok(res)
}

pub fn format_date(dt: &DateTime<Utc>) -> String {
    dt.format("%b %-d, %Y").to_string()
}

// --- Layout --------------------------------------------------------------------

pub struct Layout {
    pub title: String,
    pub description: String,
    pub user: Option<SessionUser>,
    pub path: String,
    pub asset_version: &'static str,
    /// Hide account links (used on error pages where we don't know the user).
    pub minimal: bool,
}

pub const SITE_NAME: &str = "System Design Transparent";
const DEFAULT_DESCRIPTION: &str = "Free, open, community-written explanations of how real backends work: \
    databases, networking, distributed systems, architecture and operations.";

impl Layout {
    pub fn new(ctx: &Ctx, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: DEFAULT_DESCRIPTION.into(),
            user: ctx.user.clone(),
            path: ctx.path.clone(),
            asset_version: asset_version(),
            minimal: false,
        }
    }

    pub fn minimal(title: &str) -> Self {
        Self {
            title: title.into(),
            description: DEFAULT_DESCRIPTION.into(),
            user: None,
            path: "/".into(),
            asset_version: asset_version(),
            minimal: true,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn full_title(&self) -> String {
        if self.title.is_empty() || self.title == SITE_NAME {
            SITE_NAME.to_string()
        } else {
            format!("{} · {SITE_NAME}", self.title)
        }
    }

    pub fn nav(&self, prefix: &str) -> &'static str {
        let active = if prefix == "/" {
            self.path == "/" || self.path.starts_with("/?") || self.path.starts_with("/posts")
        } else {
            self.path.starts_with(prefix)
        };
        if active { "page" } else { "false" }
    }

    pub fn login_href(&self) -> String {
        login_href(&self.path)
    }
}

pub fn login_href(next: &str) -> String {
    let encoded: String = next
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!("/login?next={encoded}")
}

// --- Post views ----------------------------------------------------------------

pub struct CardView {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub level: String,
    pub tags: Vec<String>,
    pub reading_minutes: i32,
    pub date: String,
    pub like_count: i32,
    pub upvote_count: i32,
    pub headline: Option<String>,
}

impl From<PostCard> for CardView {
    fn from(c: PostCard) -> Self {
        Self {
            date: format_date(&c.published_at),
            slug: c.slug,
            title: c.title,
            summary: c.summary,
            level: c.level,
            tags: c.tags,
            reading_minutes: c.reading_minutes,
            like_count: c.like_count,
            upvote_count: c.upvote_count,
            headline: c.headline,
        }
    }
}

pub struct Results {
    pub cards: Vec<CardView>,
    pub total: i64,
    pub page: i64,
    pub pages: i64,
    pub q: String,
    pub prev_href: Option<String>,
    pub next_href: Option<String>,
}

impl Results {
    pub fn new(cards: Vec<PostCard>, filter: &ListFilter, page_size: i64) -> Self {
        let total = cards.first().map(|c| c.total).unwrap_or(0);
        let pages = ((total + page_size - 1) / page_size).max(1);
        let page = filter.page;
        Self {
            cards: cards.into_iter().map(CardView::from).collect(),
            total,
            page,
            pages,
            q: filter.q.clone(),
            prev_href: (page > 1).then(|| format!("/?{}", filter.page_query(page - 1))),
            next_href: (page < pages).then(|| format!("/?{}", filter.page_query(page + 1))),
        }
    }
}

pub struct SortChoice {
    pub value: &'static str,
    pub label: &'static str,
    pub selected: bool,
}

pub struct FilterView {
    pub q: String,
    pub tag: String,
    pub level: String,
}

impl FilterView {
    pub fn new(f: &ListFilter) -> Self {
        Self { q: f.q.clone(), tag: f.tag.clone().unwrap_or_default(), level: f.level.clone().unwrap_or_default() }
    }
}

/// The sort dropdown. Its first option has an empty value meaning "default
/// order" (best match while searching, newest otherwise), so a search typed
/// with the dropdown untouched is ranked by relevance.
pub struct SortSelect {
    pub choices: Vec<SortChoice>,
    /// Rendered with `hx-swap-oob` inside htmx result fragments, so the
    /// dropdown's labels follow the query without re-rendering the search box.
    pub oob: bool,
}

impl SortSelect {
    pub fn new(f: &ListFilter, oob: bool) -> Self {
        let default = Sort::default_for(f.tsquery.is_some());
        let default_label = Sort::CHOICES.iter().find(|(v, _)| *v == default.as_str()).map_or("Newest", |(_, l)| *l);
        let mut choices = vec![SortChoice { value: "", label: default_label, selected: !f.explicit_sort }];
        choices.extend(
            Sort::CHOICES.iter().filter(|(value, _)| *value != default.as_str() && *value != "relevance").map(
                |&(value, label)| SortChoice { value, label, selected: f.explicit_sort && value == f.sort.as_str() },
            ),
        );
        Self { choices, oob }
    }
}

#[derive(Template)]
#[template(path = "home.html")]
pub struct HomePage {
    pub layout: Layout,
    pub filter: FilterView,
    pub sort: SortSelect,
    pub results: Results,
    pub tags: Vec<TagCount>,
    pub open_rounds: Vec<Round>,
    pub show_hero: bool,
}

#[derive(Template)]
#[template(path = "partials/results_fragment.html")]
pub struct ResultsPartial {
    pub results: Results,
    pub sort: SortSelect,
}

pub struct PostView {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub level: String,
    pub tags: Vec<String>,
    pub body_html: String,
    pub toc_html: String,
    pub reading_minutes: i32,
    pub published: String,
    pub updated: Option<String>,
}

pub struct ReactionsView {
    pub slug: String,
    pub logged_in: bool,
    pub state: ReactionState,
    pub login_href: String,
}

#[derive(Template)]
#[template(path = "post.html")]
pub struct PostPage {
    pub layout: Layout,
    pub post: PostView,
    pub reactions: ReactionsView,
    pub related: Vec<CardView>,
    pub edit_url: String,
}

#[derive(Template)]
#[template(path = "partials/reactions.html")]
pub struct ReactionsPartial {
    pub reactions: ReactionsView,
}

// --- Other pages ---------------------------------------------------------------

#[derive(Template)]
#[template(path = "tags.html")]
pub struct TagsPage {
    pub layout: Layout,
    pub tags: Vec<TagCount>,
}

pub struct TopicView {
    pub title: String,
    pub note: String,
    pub href: Option<String>,
}

pub struct SectionView {
    pub title: String,
    pub description: String,
    pub topics: Vec<TopicView>,
    pub written: usize,
}

#[derive(Template)]
#[template(path = "roadmap.html")]
pub struct RoadmapPage {
    pub layout: Layout,
    pub sections: Vec<SectionView>,
    pub written: usize,
    pub total: usize,
}

#[derive(Template)]
#[template(path = "about.html")]
pub struct AboutPage {
    pub layout: Layout,
    pub repo_url: String,
}

#[derive(Template)]
#[template(path = "auth/login.html")]
pub struct LoginPage {
    pub layout: Layout,
    pub error: Option<String>,
    pub username: String,
    pub next: String,
}

#[derive(Template)]
#[template(path = "auth/register.html")]
pub struct RegisterPage {
    pub layout: Layout,
    pub error: Option<String>,
    pub username: String,
    pub next: String,
}

#[derive(Template)]
#[template(path = "me.html")]
pub struct MePage {
    pub layout: Layout,
    pub tab: &'static str,
    pub cards: Vec<CardView>,
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub layout: Layout,
    pub status: u16,
    pub message: String,
}

// --- Voting --------------------------------------------------------------------

pub struct RoundBlock {
    pub round: Round,
    pub polls: Vec<PollView>,
}

pub struct PastRound {
    pub round: Round,
    pub winners: Vec<Winner>,
}

#[derive(Template)]
#[template(path = "vote/index.html")]
pub struct VoteIndexPage {
    pub layout: Layout,
    pub open: Vec<RoundBlock>,
    pub past: Vec<PastRound>,
    pub max_votes: i64,
    pub max_suggestions: i64,
}

#[derive(Template)]
#[template(path = "vote/round.html")]
pub struct RoundPage {
    pub layout: Layout,
    pub block: RoundBlock,
}

#[derive(Template)]
#[template(path = "partials/poll.html")]
pub struct PollPartial {
    pub pv: PollView,
}

// --- Admin ---------------------------------------------------------------------

#[derive(Template)]
#[template(path = "admin/index.html")]
pub struct AdminPage {
    pub layout: Layout,
    pub rounds: Vec<Round>,
    pub tags: Vec<TagCount>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/round.html")]
pub struct AdminRoundPage {
    pub layout: Layout,
    pub block: RoundBlock,
    pub post_slugs: Vec<String>,
    pub error: Option<String>,
}
