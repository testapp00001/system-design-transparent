//! Articles live as Markdown files in `content/posts/*.md` so anyone can
//! improve them with a normal pull request. On startup we validate every file
//! and upsert it into Postgres, which serves reads, search and filtering.

pub mod markdown;
pub mod roadmap;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

use anyhow::{Context, bail};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

pub const LEVELS: [&str; 3] = ["beginner", "intermediate", "advanced"];
const WORDS_PER_MINUTE: usize = 220;

/// Arbitrary constant identifying the "content sync" advisory lock, so two
/// app instances starting at the same time do not sync concurrently.
const SYNC_LOCK_KEY: i64 = 0x05d7_c0de;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontmatter {
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub level: String,
    pub date: toml::value::Datetime,
    #[serde(default)]
    pub updated: Option<toml::value::Datetime>,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug)]
pub struct ParsedPost {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub level: String,
    pub published_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub draft: bool,
    pub body_md: String,
    pub body_text: String,
    pub body_html: String,
    pub toc_html: String,
    pub reading_minutes: i32,
    pub content_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct TagDef {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct TagsFile {
    #[serde(rename = "tag")]
    tags: Vec<TagDef>,
}

/// Everything under `content/`, parsed and cross-validated.
pub struct Library {
    pub tags: Vec<TagDef>,
    pub posts: Vec<ParsedPost>,
    pub roadmap: roadmap::Roadmap,
}

/// Splits `+++\n<toml>\n+++\n<markdown>` into its two halves.
fn split_frontmatter(raw: &str) -> anyhow::Result<(&str, &str)> {
    let rest = raw.strip_prefix("+++\n").context("file must start with a '+++' TOML frontmatter block")?;
    let end = rest.find("\n+++").context("frontmatter block is not closed with '+++'")?;
    let fm = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    Ok((fm, body))
}

fn toml_date(dt: &toml::value::Datetime) -> anyhow::Result<DateTime<Utc>> {
    let d = dt.date.context("date must include a calendar date, e.g. 2026-10-02")?;
    let date = NaiveDate::from_ymd_opt(d.year.into(), d.month.into(), d.day.into()).context("invalid calendar date")?;
    Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

pub fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn parse_post(slug: &str, raw: &str) -> anyhow::Result<ParsedPost> {
    if !is_valid_slug(slug) {
        bail!("file name must be a lowercase-kebab-case slug, got {slug:?}");
    }
    let normalized = raw.replace("\r\n", "\n");
    let (fm_src, body) = split_frontmatter(&normalized)?;
    let fm: Frontmatter = toml::from_str(fm_src).context("invalid frontmatter")?;

    if fm.title.trim().is_empty() {
        bail!("title must not be empty");
    }
    let summary_len = fm.summary.trim().chars().count();
    if !(20..=320).contains(&summary_len) {
        bail!("summary should be 20–320 characters (it is {summary_len})");
    }
    if !LEVELS.contains(&fm.level.as_str()) {
        bail!("level must be one of {LEVELS:?}, got {:?}", fm.level);
    }
    if fm.tags.is_empty() {
        bail!("at least one tag is required");
    }
    if body.trim().is_empty() {
        bail!("article body is empty");
    }

    let published_at = toml_date(&fm.date)?;
    let updated_at = match &fm.updated {
        Some(u) => toml_date(u)?,
        None => published_at,
    };
    let rendered = markdown::render(body);

    Ok(ParsedPost {
        slug: slug.to_string(),
        title: fm.title.trim().to_string(),
        summary: fm.summary.trim().to_string(),
        tags: fm.tags,
        level: fm.level,
        published_at,
        updated_at,
        draft: fm.draft,
        body_md: body.to_string(),
        body_text: rendered.text,
        body_html: rendered.html,
        toc_html: rendered.toc_html,
        reading_minutes: rendered.word_count.div_ceil(WORDS_PER_MINUTE).max(1) as i32,
        content_hash: hex::encode(Sha256::digest(normalized.as_bytes())),
    })
}

/// Loads and validates the whole content directory. Fails with a precise
/// message (file + reason) so contributors get useful CI errors.
pub fn load_library(dir: &Path) -> anyhow::Result<Library> {
    let tags_path = dir.join("tags.toml");
    let tags_src = fs::read_to_string(&tags_path).with_context(|| format!("reading {}", tags_path.display()))?;
    let tags: TagsFile = toml::from_str(&tags_src).with_context(|| format!("parsing {}", tags_path.display()))?;
    let mut tag_slugs = HashSet::new();
    for t in &tags.tags {
        if !is_valid_slug(&t.slug) {
            bail!("{}: invalid tag slug {:?}", tags_path.display(), t.slug);
        }
        if !tag_slugs.insert(t.slug.clone()) {
            bail!("{}: duplicate tag {:?}", tags_path.display(), t.slug);
        }
    }

    let posts_dir = dir.join("posts");
    let mut posts = Vec::new();
    let mut seen = HashSet::new();
    for entry in walkdir::WalkDir::new(&posts_dir).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if !entry.file_type().is_file() || path.extension().is_none_or(|e| e != "md") || stem.starts_with('_') {
            continue;
        }
        let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let post = parse_post(stem, &raw).with_context(|| format!("in {}", path.display()))?;
        if let Some(unknown) = post.tags.iter().find(|t| !tag_slugs.contains(*t)) {
            bail!("{}: unknown tag {unknown:?} (declare it in content/tags.toml)", path.display());
        }
        if !seen.insert(post.slug.clone()) {
            bail!("{}: duplicate slug {:?}", path.display(), post.slug);
        }
        posts.push(post);
    }

    let roadmap = roadmap::Roadmap::load(&dir.join("roadmap.toml"))?;
    let published: HashSet<&str> = posts.iter().filter(|p| !p.draft).map(|p| p.slug.as_str()).collect();
    for topic in roadmap.sections.iter().flat_map(|s| &s.topics) {
        if let Some(slug) = &topic.post
            && !published.contains(slug.as_str())
        {
            bail!("roadmap.toml: topic {:?} links to unknown post {slug:?}", topic.title);
        }
    }

    Ok(Library { tags: tags.tags, posts, roadmap })
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub unpublished: u64,
}

/// Upserts the library into Postgres inside one transaction.
pub async fn sync(db: &PgPool, library: &Library) -> anyhow::Result<SyncReport> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(SYNC_LOCK_KEY).execute(&mut *tx).await?;

    for tag in &library.tags {
        sqlx::query(
            "INSERT INTO tags (slug, name, description) VALUES ($1, $2, $3)
             ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name, description = EXCLUDED.description",
        )
        .bind(&tag.slug)
        .bind(&tag.name)
        .bind(&tag.description)
        .execute(&mut *tx)
        .await?;
    }

    let existing: HashMap<String, (String, bool)> =
        sqlx::query_as::<_, (String, String, bool)>("SELECT slug, content_hash, is_published FROM posts")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|(slug, hash, published)| (slug, (hash, published)))
            .collect();

    let mut report = SyncReport::default();
    let mut live_slugs = Vec::new();
    for post in library.posts.iter().filter(|p| !p.draft) {
        live_slugs.push(post.slug.clone());
        match existing.get(&post.slug) {
            Some((hash, true)) if *hash == post.content_hash => {
                report.unchanged += 1;
                continue;
            }
            Some(_) => report.updated += 1,
            None => report.created += 1,
        }
        sqlx::query(
            "INSERT INTO posts (slug, title, summary, level, tags, body_md, body_text, body_html, toc_html,
                                reading_minutes, content_hash, published_at, updated_at, is_published,
                                search_vector)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, true,
                     setweight(to_tsvector('english', $2), 'A') ||
                     setweight(to_tsvector('english', $3), 'B') ||
                     setweight(to_tsvector('english', array_to_string($5, ' ')), 'B') ||
                     setweight(to_tsvector('english', $7), 'C'))
             ON CONFLICT (slug) DO UPDATE SET
                title = EXCLUDED.title, summary = EXCLUDED.summary, level = EXCLUDED.level,
                tags = EXCLUDED.tags, body_md = EXCLUDED.body_md, body_text = EXCLUDED.body_text,
                body_html = EXCLUDED.body_html,
                toc_html = EXCLUDED.toc_html, reading_minutes = EXCLUDED.reading_minutes,
                content_hash = EXCLUDED.content_hash, published_at = EXCLUDED.published_at,
                updated_at = EXCLUDED.updated_at, is_published = true,
                search_vector = EXCLUDED.search_vector",
        )
        .bind(&post.slug)
        .bind(&post.title)
        .bind(&post.summary)
        .bind(&post.level)
        .bind(&post.tags)
        .bind(&post.body_md)
        .bind(&post.body_text)
        .bind(&post.body_html)
        .bind(&post.toc_html)
        .bind(post.reading_minutes)
        .bind(&post.content_hash)
        .bind(post.published_at)
        .bind(post.updated_at)
        .execute(&mut *tx)
        .await?;
    }

    report.unpublished =
        sqlx::query("UPDATE posts SET is_published = false WHERE is_published AND NOT (slug = ANY($1))")
            .bind(&live_slugs)
            .execute(&mut *tx)
            .await?
            .rows_affected();

    tx.commit().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "+++\ntitle = \"Hello\"\nsummary = \"A summary that is long enough to pass.\"\ntags = [\"database\"]\nlevel = \"beginner\"\ndate = 2026-01-02\n+++\n\n## Section\n\nBody text here.\n";

    #[test]
    fn parses_frontmatter_and_body() {
        let p = parse_post("hello", SAMPLE).unwrap();
        assert_eq!(p.title, "Hello");
        assert_eq!(p.tags, vec!["database"]);
        assert_eq!(p.published_at.to_rfc3339(), "2026-01-02T00:00:00+00:00");
        assert_eq!(p.updated_at, p.published_at);
        assert!(p.body_html.contains("<h2 id=\"section\">"));
        assert_eq!(p.reading_minutes, 1);
        assert!(!p.draft);
    }

    #[test]
    fn handles_windows_line_endings() {
        let p = parse_post("hello", &SAMPLE.replace('\n', "\r\n")).unwrap();
        assert_eq!(p.title, "Hello");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_post("Bad_Slug", SAMPLE).is_err());
        assert!(parse_post("x", "no frontmatter").is_err());
        assert!(parse_post("x", &SAMPLE.replace("beginner", "expert")).is_err());
        assert!(parse_post("x", &SAMPLE.replace("level =", "lvl =")).is_err(), "unknown fields rejected");
    }

    /// Guards every pull request: the real content directory must be valid.
    #[test]
    fn repository_content_is_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("content");
        let lib = load_library(&dir).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(!lib.posts.is_empty());
    }
}
