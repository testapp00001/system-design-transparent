use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

use crate::templates::{ErrorPage, Layout};

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Page not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("You need to log in to do that")]
    Unauthorized,
    #[error("You are not allowed to do that")]
    Forbidden,
    #[error("Too many attempts. Please wait a few minutes and try again.")]
    TooManyRequests,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            AppError::Database(_) | AppError::Template(_) | AppError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        // Never leak internal error details to visitors; log them instead.
        let message = if status.is_server_error() {
            tracing::error!(error = ?self, "request failed");
            "Something went wrong on our side. Please try again.".to_string()
        } else {
            self.to_string()
        };

        let page = ErrorPage {
            layout: Layout::minimal(status.canonical_reason().unwrap_or("Error")),
            status: status.as_u16(),
            message,
        };
        match askama::Template::render(&page) {
            Ok(html) => (status, Html(html)).into_response(),
            Err(_) => (status, status.canonical_reason().unwrap_or("Error")).into_response(),
        }
    }
}
