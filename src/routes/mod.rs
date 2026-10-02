mod account;
mod admin;
mod pages;
mod posts;
mod vote;

use axum::{
    Router,
    http::{HeaderName, HeaderValue, header},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use tower_http::{
    compression::CompressionLayer, services::ServeDir, set_header::SetResponseHeaderLayer, trace::TraceLayer,
};

use crate::{auth, error::AppError, security, state::AppState};

pub fn router(state: AppState) -> Router {
    // Routes that render pages need the current user; static files do not, so
    // only this sub-router pays for the session lookup.
    let pages = Router::new()
        .route("/", get(posts::index))
        .route("/posts/{slug}", get(posts::show))
        .route("/posts/{slug}/react/{kind}", post(posts::react))
        .route("/tags", get(pages::tags))
        .route("/roadmap", get(pages::roadmap))
        .route("/about", get(pages::about))
        .route("/vote", get(vote::index))
        .route("/vote/rounds/{id}", get(vote::round))
        .route("/vote/polls/{id}/suggestions", post(vote::suggest))
        .route("/vote/suggestions/{id}/vote", post(vote::vote))
        .route("/login", get(account::login_form).post(account::login))
        .route("/register", get(account::register_form).post(account::register))
        .route("/logout", post(account::logout))
        .route("/me", get(account::me))
        .route("/admin", get(admin::index))
        .route("/admin/rounds", post(admin::create_round))
        .route("/admin/rounds/{id}", get(admin::round))
        .route("/admin/rounds/{id}/close", post(admin::close_round))
        .route("/admin/suggestions/{id}/hidden", post(admin::set_hidden))
        .route("/admin/suggestions/{id}/fulfill", post(admin::fulfill))
        .layer(middleware::from_fn_with_state(state.clone(), auth::load_user));

    let static_files = Router::new().fallback_service(ServeDir::new(&state.config.static_dir)).layer(
        SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=604800")),
    );

    Router::new()
        .merge(pages)
        .route("/healthz", get(pages::healthz))
        .route("/feed.xml", get(pages::feed))
        .route("/sitemap.xml", get(pages::sitemap))
        .route("/robots.txt", get(pages::robots))
        .route("/favicon.ico", get(|| async { Redirect::permanent("/static/favicon.svg") }))
        .nest("/static", static_files)
        .fallback(not_found)
        .layer(middleware::from_fn(security::cross_origin_guard))
        .layer(middleware::from_fn(security::security_headers))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn not_found() -> Response {
    AppError::NotFound.into_response()
}

/// After a non-htmx form POST, send the browser back with 303 See Other so a
/// refresh does not re-submit the form (the Post/Redirect/Get pattern).
fn see_other(location: &str) -> Response {
    Redirect::to(location).into_response()
}

/// Tell htmx to do a full-page navigation (used e.g. to send anonymous
/// visitors to the login page from a button click).
fn hx_redirect(location: &str) -> Response {
    let mut res = ().into_response();
    if let Ok(v) = HeaderValue::from_str(location) {
        res.headers_mut().insert(HeaderName::from_static("hx-redirect"), v);
    }
    res
}

/// Parsed `on=1` / `on=0` form field used by idempotent set-state endpoints.
#[derive(serde::Deserialize)]
struct OnForm {
    on: String,
}

impl OnForm {
    fn on(&self) -> bool {
        self.on == "1" || self.on == "true"
    }
}
