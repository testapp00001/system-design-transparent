//! End-to-end tests: real router, real Postgres (a fresh database per test,
//! created by `#[sqlx::test]`). Requires `DATABASE_URL` pointing at a server
//! where the user may create databases, e.g.
//! `DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test`.

use std::path::Path;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use sqlx::PgPool;
use system_design_transparent::{
    MIGRATOR,
    config::Config,
    content::{self, Library},
    ip::hash_ip,
    router,
    state::AppState,
    users, votes,
};
use tower::ServiceExt;

struct TestApp {
    router: Router,
    db: PgPool,
    library: Library,
    config: Config,
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Res {
    fn cookie(&self) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("sdt_session="))
            .map(|c| c.split(';').next().unwrap().to_string())
    }

    fn location(&self) -> &str {
        self.headers.get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("")
    }
}

#[derive(Default, Clone, Copy)]
struct Opts<'a> {
    cookie: Option<&'a str>,
    ip: Option<&'a str>,
    htmx: bool,
}

impl TestApp {
    async fn new(db: PgPool) -> Self {
        let config = Config::for_tests();
        let library = content::load_library(Path::new(&config.content_dir)).expect("content is valid");
        content::sync(&db, &library).await.expect("sync content");
        let roadmap = content::load_library(Path::new(&config.content_dir)).unwrap().roadmap;
        let state = AppState::new(db.clone(), config.clone(), roadmap);
        Self { router: router(state), db, library, config }
    }

    async fn send(&self, method: &str, path: &str, body: Option<&str>, o: Opts<'_>) -> Res {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(c) = o.cookie {
            req = req.header(header::COOKIE, c);
        }
        // Config::for_tests trusts one proxy hop, so this sets the client IP.
        req = req.header("x-forwarded-for", o.ip.unwrap_or("198.51.100.1"));
        if o.htmx {
            req = req.header("hx-request", "true");
        }
        let req = match body {
            Some(b) => {
                req.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded").body(Body::from(b.to_string()))
            }
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Res { status, headers, body: String::from_utf8_lossy(&bytes).into_owned() }
    }

    async fn get(&self, path: &str, o: Opts<'_>) -> Res {
        self.send("GET", path, None, o).await
    }

    async fn post(&self, path: &str, body: &str, o: Opts<'_>) -> Res {
        self.send("POST", path, Some(body), o).await
    }

    /// Registers a user and returns their session cookie.
    async fn register(&self, username: &str) -> String {
        let body = format!("username={username}&password=correct-horse&password_confirm=correct-horse");
        let res = self.post("/register", &body, Opts::default()).await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        res.cookie().expect("session cookie")
    }

    fn first_post(&self) -> &content::ParsedPost {
        self.library.posts.iter().find(|p| !p.draft).expect("at least one post")
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn home_search_and_post_pages(db: PgPool) {
    let app = TestApp::new(db).await;
    let post = app.first_post();

    let published = app.library.posts.iter().filter(|p| !p.draft).count();
    let res = app.get("/", Opts::default()).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("<html"));
    assert!(res.body.contains(&format!("{published} articles")), "home shows the total");

    // Every post is reachable by paging through the list.
    let pages = published.div_ceil(12);
    let mut seen = 0;
    for page in 1..=pages {
        let res = app.get(&format!("/?page={page}"), Opts::default()).await;
        seen += res.body.matches(r#"class="card-title""#).count();
    }
    assert_eq!(seen, published);

    // htmx search returns only the results fragment.
    let word = post.title.split_whitespace().max_by_key(|w| w.len()).unwrap();
    let q: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
    let res = app.get(&format!("/?q={q}"), Opts { htmx: true, ..Default::default() }).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.body.contains("<html"), "fragment only");
    assert!(res.body.contains(r#"id="results""#));
    assert!(res.body.contains(&format!("/posts/{}", post.slug)), "search for {q:?} should find {}", post.slug);

    let res = app.get(&format!("/posts/{}", post.slug), Opts::default()).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.headers.get(header::CONTENT_SECURITY_POLICY).is_some());

    assert_eq!(app.get("/posts/does-not-exist", Opts::default()).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get("/no/such/page", Opts::default()).await.status, StatusCode::NOT_FOUND);

    for path in ["/tags", "/roadmap", "/about", "/vote", "/feed.xml", "/sitemap.xml", "/healthz", "/robots.txt"] {
        assert_eq!(app.get(path, Opts::default()).await.status, StatusCode::OK, "{path}");
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn register_login_logout(db: PgPool) {
    let app = TestApp::new(db).await;
    let cookie = app.register("alice").await;

    let res = app.get("/", Opts { cookie: Some(&cookie), ..Default::default() }).await;
    assert!(res.body.contains("@alice"));

    // Usernames are case-insensitively unique.
    let res = app
        .post("/register", "username=ALICE&password=whatever123&password_confirm=whatever123", Opts::default())
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);

    let res = app.post("/logout", "", Opts { cookie: Some(&cookie), ..Default::default() }).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let res = app.get("/", Opts { cookie: Some(&cookie), ..Default::default() }).await;
    assert!(!res.body.contains("@alice"), "session is gone after logout");

    let res = app.post("/login", "username=alice&password=wrong-password", Opts::default()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = app.post("/login", "username=nobody&password=wrong-password", Opts::default()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let res = app.post("/login", "username=Alice&password=correct-horse&next=%2Ftags", Opts::default()).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.location(), "/tags");
    assert!(res.cookie().is_some());

    // Open redirects are refused.
    let res =
        app.post("/login", "username=alice&password=correct-horse&next=%2F%2Fevil.example", Opts::default()).await;
    assert_eq!(res.location(), "/");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn reactions_are_idempotent(db: PgPool) {
    let app = TestApp::new(db).await;
    let slug = app.first_post().slug.clone();
    let path = format!("/posts/{slug}/react/like");

    // Anonymous visitors are sent to the login page.
    let res = app.post(&path, "on=1", Opts { htmx: true, ..Default::default() }).await;
    assert!(res.headers.get("hx-redirect").is_some());

    let cookie = app.register("bob").await;
    let o = Opts { cookie: Some(&cookie), htmx: true, ..Default::default() };
    for _ in 0..3 {
        let res = app.post(&path, "on=1", o).await;
        assert_eq!(res.status, StatusCode::OK);
        assert!(res.body.contains(r#"id="reactions""#));
    }
    let count: i32 = sqlx::query_scalar("SELECT like_count FROM posts WHERE slug = $1")
        .bind(&slug)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count, 1, "liking three times still counts once");

    app.post(&format!("/posts/{slug}/react/save"), "on=1", o).await;
    let res = app.get("/me?tab=saved", o).await;
    assert!(res.body.contains(&format!("/posts/{slug}")));

    for _ in 0..2 {
        app.post(&path, "on=0", o).await;
    }
    let count: i32 = sqlx::query_scalar("SELECT like_count FROM posts WHERE slug = $1")
        .bind(&slug)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);

    assert_eq!(app.post(&format!("/posts/{slug}/react/explode"), "on=1", o).await.status, StatusCode::NOT_FOUND);
}

async fn open_test_round(app: &TestApp) -> (i64, i64) {
    let admin_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash, is_admin) VALUES ('root', 'x', true) RETURNING id",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    let round_id = votes::create_round(
        &app.db,
        "Test round",
        "",
        &["database".to_string(), "networking".to_string()],
        chrono::Duration::days(3),
        admin_id,
    )
    .await
    .map_err(|e| e.to_string())
    .unwrap();
    let poll_id: i64 = sqlx::query_scalar("SELECT id FROM vote_polls WHERE round_id = $1 AND tag_slug = 'database'")
        .bind(round_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    (round_id, poll_id)
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn vote_game_limits_and_winner(db: PgPool) {
    let app = TestApp::new(db).await;
    let (round_id, poll_id) = open_test_round(&app).await;
    let a = Opts { ip: Some("203.0.113.10"), htmx: true, ..Default::default() };
    let b = Opts { ip: Some("203.0.113.20"), htmx: true, ..Default::default() };
    let suggest = |title: &str| format!("title={}&details=", title.replace(' ', "+"));

    // Each network may suggest 3 topics per poll.
    for t in ["How VACUUM works", "MVCC explained", "Index-only scans"] {
        let res = app.post(&format!("/vote/polls/{poll_id}/suggestions"), &suggest(t), a).await;
        assert!(res.body.contains("Your suggestion is in"), "{}", res.body);
    }
    let res = app.post(&format!("/vote/polls/{poll_id}/suggestions"), &suggest("One too many"), a).await;
    assert!(res.body.contains("already made 3 suggestions"), "{}", res.body);
    // Duplicates (case-insensitive) are rejected.
    let res = app.post(&format!("/vote/polls/{poll_id}/suggestions"), &suggest("mvcc EXPLAINED"), b).await;
    assert!(res.body.contains("already suggested"), "{}", res.body);
    let res = app.post(&format!("/vote/polls/{poll_id}/suggestions"), &suggest("WAL and checkpoints"), b).await;
    assert!(res.body.contains("Your suggestion is in"));

    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM suggestions WHERE poll_id = $1 ORDER BY id")
        .bind(poll_id)
        .fetch_all(&app.db)
        .await
        .unwrap();
    assert_eq!(ids.len(), 4);

    // Network A: 3 votes, the 4th is refused; repeating a vote is a no-op.
    for id in &ids[..3] {
        app.post(&format!("/vote/suggestions/{id}/vote"), "on=1", a).await;
    }
    app.post(&format!("/vote/suggestions/{}/vote", ids[0]), "on=1", a).await;
    let res = app.post(&format!("/vote/suggestions/{}/vote", ids[3]), "on=1", a).await;
    assert!(res.body.contains("used all 3 of your votes"), "{}", res.body);
    // Removing one frees a vote.
    app.post(&format!("/vote/suggestions/{}/vote", ids[2]), "on=0", a).await;
    let res = app.post(&format!("/vote/suggestions/{}/vote", ids[3]), "on=1", a).await;
    assert!(!res.body.contains("alert-error"), "{}", res.body);

    // Network B votes for ids[1] too, making it the winner.
    app.post(&format!("/vote/suggestions/{}/vote", ids[1]), "on=1", b).await;

    let counts: Vec<i32> = sqlx::query_scalar("SELECT vote_count FROM suggestions WHERE poll_id = $1 ORDER BY id")
        .bind(poll_id)
        .fetch_all(&app.db)
        .await
        .unwrap();
    assert_eq!(counts, vec![1, 2, 0, 1]);

    // IPv6 addresses in the same /64 count as one network.
    let v6a = Opts { ip: Some("2001:db8:1:2::1"), htmx: true, ..Default::default() };
    let v6b = Opts { ip: Some("2001:db8:1:2:ffff::9"), htmx: true, ..Default::default() };
    app.post(&format!("/vote/suggestions/{}/vote", ids[3]), "on=1", v6a).await;
    app.post(&format!("/vote/suggestions/{}/vote", ids[3]), "on=1", v6b).await;
    let c: i32 = sqlx::query_scalar("SELECT vote_count FROM suggestions WHERE id = $1")
        .bind(ids[3])
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(c, 2, "second address in the same /64 is the same voter");

    // Close the round: winners are frozen and voting stops.
    votes::close_round_now(&app.db, round_id).await.unwrap();
    let winner: Option<i64> = sqlx::query_scalar("SELECT winner_suggestion_id FROM vote_polls WHERE id = $1")
        .bind(poll_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(winner, Some(ids[1]));
    let res = app.post(&format!("/vote/suggestions/{}/vote", ids[0]), "on=1", b).await;
    assert!(res.body.contains("This round is closed"), "{}", res.body);

    let res = app.get("/vote", Opts::default()).await;
    assert!(res.body.contains("Past rounds") && res.body.contains("MVCC explained"));
}

/// The classic check-then-insert race: fire many votes from the same network
/// at once. The advisory lock in `set_vote` must keep the total at the cap.
#[sqlx::test(migrator = "MIGRATOR")]
async fn concurrent_votes_cannot_exceed_the_cap(db: PgPool) {
    let app = TestApp::new(db).await;
    let (_, poll_id) = open_test_round(&app).await;
    let author = hash_ip(&app.config.ip_hash_secret, "192.0.2.1".parse().unwrap());
    let mut ids = Vec::new();
    for i in 0..8 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO suggestions (poll_id, title, author_ip_hash) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(poll_id)
        .bind(format!("Suggestion number {i}"))
        .bind(&author)
        .fetch_one(&app.db)
        .await
        .unwrap();
        ids.push(id);
    }

    let voter = hash_ip(&app.config.ip_hash_secret, "192.0.2.99".parse().unwrap());
    let tasks: Vec<_> = ids
        .iter()
        .map(|&id| {
            let (db, cfg, voter) = (app.db.clone(), app.config.clone(), voter.clone());
            tokio::spawn(async move { votes::set_vote(&db, &cfg, id, &voter, true).await.is_ok() })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        if t.await.unwrap() {
            ok += 1;
        }
    }
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM suggestion_votes WHERE poll_id = $1")
        .bind(poll_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(ok, 3);
    assert_eq!(stored, 3);
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn admin_pages_are_protected(db: PgPool) {
    let app = TestApp::new(db).await;
    assert_eq!(app.get("/admin", Opts::default()).await.status, StatusCode::UNAUTHORIZED);

    let cookie = app.register("carol").await;
    let o = Opts { cookie: Some(&cookie), ..Default::default() };
    assert_eq!(app.get("/admin", o).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.post("/admin/rounds", "title=x&duration_hours=24&tags=database", o).await.status,
        StatusCode::FORBIDDEN
    );

    assert!(users::set_admin(&app.db, "carol", true).await.unwrap());
    assert_eq!(app.get("/admin", o).await.status, StatusCode::OK);

    let res = app.post("/admin/rounds", "title=Week+1&duration_hours=72&tags=database&tags=networking", o).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(res.location().starts_with("/admin/rounds/"));
    let polls: i64 = sqlx::query_scalar("SELECT count(*) FROM vote_polls").fetch_one(&app.db).await.unwrap();
    assert_eq!(polls, 2);

    let res = app.post("/admin/rounds", "title=Bad&duration_hours=72&tags=not-a-tag", o).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);

    let res = app.get("/", Opts::default()).await;
    assert!(res.body.contains("Voting is open"), "home page shows the open round");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn cross_site_posts_are_rejected(db: PgPool) {
    let app = TestApp::new(db).await;
    let req = Request::builder()
        .method("POST")
        .uri("/vote/suggestions/1/vote")
        .header("sec-fetch-site", "cross-site")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("on=1"))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn content_sync_is_incremental(db: PgPool) {
    let app = TestApp::new(db).await;
    // A second sync of unchanged files touches nothing.
    let report = content::sync(&app.db, &app.library).await.unwrap();
    assert_eq!(report.created + report.updated, 0);
    assert_eq!(report.unchanged, app.library.posts.iter().filter(|p| !p.draft).count());
}
