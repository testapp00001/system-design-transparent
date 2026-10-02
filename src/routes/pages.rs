use std::collections::HashSet;

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};

use crate::{
    error::AppResult,
    posts,
    state::AppState,
    templates::{AboutPage, Ctx, Layout, RoadmapPage, SITE_NAME, SectionView, TagsPage, TopicView, render},
    util::escape_html,
};

pub async fn tags(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    render(&TagsPage { layout: Layout::new(&ctx, "Tags"), tags: posts::tags_with_counts(&state.db).await? })
}

pub async fn roadmap(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    let live: HashSet<String> = posts::published_slugs(&state.db).await?.into_iter().collect();
    let sections: Vec<SectionView> = state
        .roadmap
        .sections
        .iter()
        .map(|s| {
            let topics: Vec<TopicView> = s
                .topics
                .iter()
                .map(|t| TopicView {
                    title: t.title.clone(),
                    note: t.note.clone(),
                    href: t.post.as_ref().filter(|slug| live.contains(*slug)).map(|slug| format!("/posts/{slug}")),
                })
                .collect();
            SectionView {
                written: topics.iter().filter(|t| t.href.is_some()).count(),
                title: s.title.clone(),
                description: s.description.clone(),
                topics,
            }
        })
        .collect();
    let written = sections.iter().map(|s| s.written).sum();
    let total = sections.iter().map(|s| s.topics.len()).sum();
    render(&RoadmapPage {
        layout: Layout::new(&ctx, "Roadmap")
            .with_description("A map of what backend engineers deal with in production, from databases to deployment."),
        sections,
        written,
        total,
    })
}

pub async fn about(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    render(&AboutPage { layout: Layout::new(&ctx, "About"), repo_url: state.config.repo_url.clone() })
}

/// Liveness/readiness probe for Docker, Kubernetes and load balancers. It
/// checks the database too, so a node that lost its DB connection is taken
/// out of rotation.
pub async fn healthz(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            tracing::warn!(error = ?e, "health check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
        }
    }
}

#[derive(sqlx::FromRow)]
struct FeedItem {
    slug: String,
    title: String,
    summary: String,
    published_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Atom feed so people can follow new articles in any feed reader.
pub async fn feed(State(state): State<AppState>) -> AppResult<Response> {
    let items: Vec<FeedItem> = sqlx::query_as(
        "SELECT slug, title, summary, published_at, updated_at FROM posts
         WHERE is_published ORDER BY published_at DESC LIMIT 30",
    )
    .fetch_all(&state.db)
    .await?;
    let base = state.config.public_url.trim_end_matches('/');
    let updated = items.iter().map(|i| i.updated_at).max().unwrap_or_else(Utc::now);

    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\n\
         <title>{}</title>\n<link href=\"{base}/\"/>\n<link rel=\"self\" href=\"{base}/feed.xml\"/>\n\
         <id>{base}/</id>\n<updated>{}</updated>\n",
        escape_html(SITE_NAME),
        updated.to_rfc3339()
    );
    for i in &items {
        xml.push_str(&format!(
            "<entry>\n<title>{}</title>\n<link href=\"{base}/posts/{slug}\"/>\n<id>{base}/posts/{slug}</id>\n\
             <published>{}</published>\n<updated>{}</updated>\n<summary>{}</summary>\n</entry>\n",
            escape_html(&i.title),
            i.published_at.to_rfc3339(),
            i.updated_at.to_rfc3339(),
            escape_html(&i.summary),
            slug = i.slug,
        ));
    }
    xml.push_str("</feed>\n");
    Ok(([(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")], xml).into_response())
}

/// Lets search engines discover every article — the whole point is that
/// people find this knowledge without already knowing it exists.
pub async fn sitemap(State(state): State<AppState>) -> AppResult<Response> {
    let items: Vec<(String, DateTime<Utc>)> =
        sqlx::query_as("SELECT slug, updated_at FROM posts WHERE is_published ORDER BY slug")
            .fetch_all(&state.db)
            .await?;
    let base = state.config.public_url.trim_end_matches('/');
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for path in ["/", "/roadmap", "/tags", "/vote", "/about"] {
        xml.push_str(&format!("<url><loc>{base}{path}</loc></url>\n"));
    }
    for (slug, updated) in items {
        xml.push_str(&format!(
            "<url><loc>{base}/posts/{slug}</loc><lastmod>{}</lastmod></url>\n",
            updated.format("%Y-%m-%d")
        ));
    }
    xml.push_str("</urlset>\n");
    Ok(([(header::CONTENT_TYPE, "application/xml; charset=utf-8")], xml).into_response())
}

pub async fn robots(State(state): State<AppState>) -> Response {
    let base = state.config.public_url.trim_end_matches('/');
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("User-agent: *\nDisallow: /admin\nDisallow: /me\n\nSitemap: {base}/sitemap.xml\n"),
    )
        .into_response()
}
