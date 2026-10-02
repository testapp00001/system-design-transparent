//! Username/password accounts with server-side sessions.
//!
//! Design notes (also covered in the "Authentication basics" article):
//! - Passwords are hashed with Argon2id, on a blocking thread because the
//!   hash is intentionally CPU-expensive and would otherwise stall the async
//!   runtime.
//! - The session cookie contains 256 random bits. The database stores only
//!   SHA-256(token), so reading the sessions table does not let anyone log in.
//! - Cookies are HttpOnly (no JS access) and SameSite=Lax (not sent on
//!   cross-site POSTs), with an extra Origin check in `security.rs`.

use std::sync::OnceLock;

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::{
    extract::{FromRequestParts, Request, State},
    http::request::Parts,
    middleware::Next,
    response::Response,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{error::AppError, state::AppState};

pub const SESSION_COOKIE: &str = "sdt_session";
pub const SESSION_TTL_DAYS: i64 = 30;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
}

// --- Passwords ---------------------------------------------------------------

pub async fn hash_password(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(|e| anyhow::anyhow!("hashing password: {e}"))
    })
    .await?
}

pub async fn verify_password(password: String, phc: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&phc)
            .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// A real Argon2 hash of a random password. When someone logs in with an
/// unknown username we still verify against this, so the response time does
/// not reveal which usernames exist.
pub async fn dummy_hash() -> String {
    static DUMMY: OnceLock<String> = OnceLock::new();
    if let Some(h) = DUMMY.get() {
        return h.clone();
    }
    let mut random = [0u8; 16];
    rand::fill(&mut random[..]);
    let h = hash_password(hex::encode(random)).await.unwrap_or_default();
    DUMMY.get_or_init(|| h).clone()
}

// --- Sessions ----------------------------------------------------------------

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Creates a session row and returns the raw token to put in the cookie.
pub async fn create_session(db: &PgPool, user_id: i64) -> Result<String, sqlx::Error> {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes[..]);
    let token = hex::encode(bytes);
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(hash_token(&token))
        .bind(user_id)
        .bind(Utc::now() + Duration::days(SESSION_TTL_DAYS))
        .execute(db)
        .await?;
    Ok(token)
}

pub async fn find_session_user(db: &PgPool, token: &str) -> Result<Option<SessionUser>, sqlx::Error> {
    sqlx::query_as(
        "SELECT u.id, u.username, u.is_admin
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > now()",
    )
    .bind(hash_token(token))
    .fetch_optional(db)
    .await
}

pub async fn delete_session(db: &PgPool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(hash_token(token)).execute(db).await?;
    Ok(())
}

pub fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(SESSION_TTL_DAYS))
        .build()
}

pub fn removal_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::ZERO)
        .build()
}

// --- Request plumbing --------------------------------------------------------

/// The logged-in user for this request, if any. Inserted by [`load_user`].
#[derive(Debug, Clone, Default)]
pub struct CurrentUser(pub Option<SessionUser>);

/// Middleware: resolve the session cookie once per request and stash the user
/// in request extensions so handlers and extractors can read it for free.
pub async fn load_user(State(state): State<AppState>, jar: CookieJar, mut req: Request, next: Next) -> Response {
    let user = match jar.get(SESSION_COOKIE) {
        Some(cookie) if !cookie.value().is_empty() => match find_session_user(&state.db, cookie.value()).await {
            Ok(user) => user,
            Err(e) => {
                tracing::error!(error = ?e, "session lookup failed");
                None
            }
        },
        _ => None,
    };
    req.extensions_mut().insert(CurrentUser(user));
    next.run(req).await
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.get::<CurrentUser>().cloned().unwrap_or_default())
    }
}

/// Extractor that rejects anonymous visitors.
pub struct RequireUser(pub SessionUser);

impl<S: Send + Sync> FromRequestParts<S> for RequireUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await.unwrap_or_default();
        user.map(RequireUser).ok_or(AppError::Unauthorized)
    }
}

/// Extractor that only lets administrators through.
pub struct RequireAdmin(pub SessionUser);

impl<S: Send + Sync> FromRequestParts<S> for RequireAdmin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let RequireUser(user) = RequireUser::from_request_parts(parts, state).await?;
        if user.is_admin { Ok(RequireAdmin(user)) } else { Err(AppError::Forbidden) }
    }
}

// --- Validation --------------------------------------------------------------

pub fn validate_username(username: &str) -> Result<(), String> {
    let len = username.chars().count();
    if !(3..=32).contains(&len) {
        return Err("Username must be 3–32 characters.".into());
    }
    if !username.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err("Username may only contain letters, numbers, '_' and '-'.".into());
    }
    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), String> {
    let len = password.chars().count();
    if len < 8 {
        return Err("Password must be at least 8 characters.".into());
    }
    // Argon2 cost grows with input; cap it so nobody can submit a 10 MB password.
    if len > 128 {
        return Err("Password must be at most 128 characters.".into());
    }
    Ok(())
}

/// Only allow redirects to local paths, never `//evil.example` or full URLs.
pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') => n.to_string(),
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_rules() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username("alice_01").is_ok());
        assert!(validate_username("bad name").is_err());
        assert!(validate_username("émile").is_err());
    }

    #[test]
    fn next_param_is_local_only() {
        assert_eq!(safe_next(Some("/posts/x")), "/posts/x");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("/\\evil.example")), "/");
        assert_eq!(safe_next(None), "/");
    }
}
