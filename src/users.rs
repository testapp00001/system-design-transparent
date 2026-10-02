use sqlx::PgPool;

pub enum CreateUserError {
    UsernameTaken,
    Database(sqlx::Error),
}

pub async fn create(db: &PgPool, username: &str, password_hash: &str) -> Result<i64, CreateUserError> {
    sqlx::query_scalar("INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id")
        .bind(username)
        .bind(password_hash)
        .fetch_one(db)
        .await
        .map_err(|e| match &e {
            // 23505 = unique_violation: let the database enforce uniqueness
            // instead of a racy "SELECT then INSERT".
            sqlx::Error::Database(d) if d.code().as_deref() == Some("23505") => CreateUserError::UsernameTaken,
            _ => CreateUserError::Database(e),
        })
}

/// Returns `(id, password_hash)` for a username, case-insensitively.
pub async fn find_credentials(db: &PgPool, username: &str) -> Result<Option<(i64, String)>, sqlx::Error> {
    sqlx::query_as("SELECT id, password_hash FROM users WHERE lower(username) = lower($1)")
        .bind(username)
        .fetch_optional(db)
        .await
}

pub async fn set_admin(db: &PgPool, username: &str, is_admin: bool) -> Result<bool, sqlx::Error> {
    let rows = sqlx::query("UPDATE users SET is_admin = $2 WHERE lower(username) = lower($1)")
        .bind(username)
        .bind(is_admin)
        .execute(db)
        .await?
        .rows_affected();
    Ok(rows == 1)
}
