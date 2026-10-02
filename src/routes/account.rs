use std::time::Duration;

use axum::{
    Form,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use super::see_other;
use crate::{
    auth::{self, safe_next, validate_password, validate_username},
    error::{AppError, AppResult},
    ip::{ClientIp, network_key},
    posts::{self, Reaction},
    state::AppState,
    templates::{CardView, Ctx, Layout, LoginPage, MePage, RegisterPage, render},
    users::{self, CreateUserError},
};

#[derive(Deserialize)]
pub struct NextParam {
    next: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
    next: Option<String>,
}

#[derive(Deserialize)]
pub struct RegisterForm {
    username: String,
    password: String,
    password_confirm: String,
    next: Option<String>,
}

const LOGIN_LIMIT: (u32, Duration) = (10, Duration::from_secs(10 * 60));
const REGISTER_LIMIT: (u32, Duration) = (5, Duration::from_secs(60 * 60));

pub async fn login_form(ctx: Ctx, Query(q): Query<NextParam>) -> AppResult<Response> {
    let next = safe_next(q.next.as_deref());
    if ctx.user.is_some() {
        return Ok(see_other(&next));
    }
    render(&LoginPage { layout: Layout::new(&ctx, "Log in"), error: None, username: String::new(), next })
}

pub async fn login(
    State(state): State<AppState>,
    ctx: Ctx,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    let next = safe_next(form.next.as_deref());
    let fail = |msg: &str, status: StatusCode| -> AppResult<Response> {
        let page = LoginPage {
            layout: Layout::new(&ctx, "Log in"),
            error: Some(msg.to_string()),
            username: form.username.clone(),
            next: next.clone(),
        };
        Ok((status, render(&page)?).into_response())
    };

    if !state.limiter.check(&format!("login:{}", network_key(ip)), LOGIN_LIMIT.0, LOGIN_LIMIT.1) {
        return fail(&AppError::TooManyRequests.to_string(), StatusCode::TOO_MANY_REQUESTS);
    }
    if form.password.chars().count() > 128 {
        return fail("Invalid username or password.", StatusCode::UNAUTHORIZED);
    }

    let creds = users::find_credentials(&state.db, form.username.trim()).await?;
    // Always run one Argon2 verification, even for unknown usernames, so
    // response timing does not reveal which accounts exist.
    let (user_id, hash) = match creds {
        Some((id, hash)) => (Some(id), hash),
        None => (None, auth::dummy_hash().await),
    };
    let ok = auth::verify_password(form.password.clone(), hash).await;
    let Some(user_id) = user_id.filter(|_| ok) else {
        return fail("Invalid username or password.", StatusCode::UNAUTHORIZED);
    };

    let token = auth::create_session(&state.db, user_id).await?;
    let jar = jar.add(auth::session_cookie(token, state.config.cookie_secure));
    Ok((jar, see_other(&next)).into_response())
}

pub async fn register_form(ctx: Ctx, Query(q): Query<NextParam>) -> AppResult<Response> {
    let next = safe_next(q.next.as_deref());
    if ctx.user.is_some() {
        return Ok(see_other(&next));
    }
    render(&RegisterPage { layout: Layout::new(&ctx, "Create account"), error: None, username: String::new(), next })
}

pub async fn register(
    State(state): State<AppState>,
    ctx: Ctx,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    Form(form): Form<RegisterForm>,
) -> AppResult<Response> {
    let next = safe_next(form.next.as_deref());
    let username = form.username.trim().to_string();
    let fail = |msg: String, status: StatusCode| -> AppResult<Response> {
        let page = RegisterPage {
            layout: Layout::new(&ctx, "Create account"),
            error: Some(msg),
            username: username.clone(),
            next: next.clone(),
        };
        Ok((status, render(&page)?).into_response())
    };

    if let Err(msg) = validate_username(&username).and_then(|_| validate_password(&form.password)) {
        return fail(msg, StatusCode::UNPROCESSABLE_ENTITY);
    }
    if form.password != form.password_confirm {
        return fail("Passwords do not match.".into(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    if !state.limiter.check(&format!("register:{}", network_key(ip)), REGISTER_LIMIT.0, REGISTER_LIMIT.1) {
        return fail(AppError::TooManyRequests.to_string(), StatusCode::TOO_MANY_REQUESTS);
    }

    let hash = auth::hash_password(form.password.clone()).await?;
    let user_id = match users::create(&state.db, &username, &hash).await {
        Ok(id) => id,
        Err(CreateUserError::UsernameTaken) => {
            return fail("That username is already taken.".into(), StatusCode::CONFLICT);
        }
        Err(CreateUserError::Database(e)) => return Err(e.into()),
    };

    let token = auth::create_session(&state.db, user_id).await?;
    let jar = jar.add(auth::session_cookie(token, state.config.cookie_secure));
    Ok((jar, see_other(&next)).into_response())
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> AppResult<Response> {
    if let Some(cookie) = jar.get(auth::SESSION_COOKIE) {
        auth::delete_session(&state.db, cookie.value()).await?;
    }
    let jar = jar.add(auth::removal_cookie(state.config.cookie_secure));
    Ok((jar, see_other("/")).into_response())
}

#[derive(Deserialize)]
pub struct MeParams {
    tab: Option<String>,
}

pub async fn me(State(state): State<AppState>, ctx: Ctx, Query(q): Query<MeParams>) -> AppResult<Response> {
    let Some(user) = ctx.user.clone() else {
        return Ok(see_other(&crate::templates::login_href("/me")));
    };
    let (tab, kind) = match q.tab.as_deref() {
        Some("liked") => ("liked", Reaction::Like),
        Some("upvoted") => ("upvoted", Reaction::Upvote),
        _ => ("saved", Reaction::Save),
    };
    let cards = posts::list_for_user(&state.db, user.id, kind).await?;
    render(&MePage {
        layout: Layout::new(&ctx, format!("@{}", user.username)),
        tab,
        cards: cards.into_iter().map(CardView::from).collect(),
    })
}
