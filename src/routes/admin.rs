use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum_extra::extract::Form;
use chrono::Duration;
use serde::Deserialize;

use super::see_other;
use crate::{
    auth::RequireAdmin,
    error::{AppError, AppResult},
    posts,
    state::AppState,
    templates::{AdminPage, AdminRoundPage, Ctx, Layout, RoundBlock, render},
    votes::{self, VoteError},
};

pub async fn index(State(state): State<AppState>, ctx: Ctx, _admin: RequireAdmin) -> AppResult<Response> {
    admin_page(&state, &ctx, None).await
}

async fn admin_page(state: &AppState, ctx: &Ctx, error: Option<String>) -> AppResult<Response> {
    let status = if error.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    let page = AdminPage {
        layout: Layout::new(ctx, "Admin"),
        rounds: votes::all_rounds(&state.db).await?,
        tags: posts::tags_with_counts(&state.db).await?,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct NewRoundForm {
    title: String,
    #[serde(default)]
    description: String,
    /// Repeated `tags=a&tags=b` checkboxes; axum-extra's Form handles repeats.
    #[serde(default)]
    tags: Vec<String>,
    duration_hours: i64,
}

pub async fn create_round(
    State(state): State<AppState>,
    ctx: Ctx,
    RequireAdmin(admin): RequireAdmin,
    Form(form): Form<NewRoundForm>,
) -> AppResult<Response> {
    // try_hours: Duration::hours panics on absurd values; MAX then fails the
    // 1 hour – 31 days validation with a normal error message.
    let duration = Duration::try_hours(form.duration_hours).unwrap_or(Duration::MAX);
    match votes::create_round(&state.db, &form.title, &form.description, &form.tags, duration, admin.id).await {
        Ok(id) => Ok(see_other(&format!("/admin/rounds/{id}"))),
        Err(VoteError::Database(e)) => Err(e.into()),
        Err(e) => admin_page(&state, &ctx, Some(e.to_string())).await,
    }
}

pub async fn round(
    State(state): State<AppState>,
    ctx: Ctx,
    _admin: RequireAdmin,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    round_page(&state, &ctx, id, None).await
}

async fn round_page(state: &AppState, ctx: &Ctx, id: i64, error: Option<String>) -> AppResult<Response> {
    let round = votes::get_round(&state.db, id).await?.ok_or(AppError::NotFound)?;
    // Admins see hidden suggestions too, so they can un-hide them.
    let polls = votes::polls_for_round(&state.db, &state.config, &round, &[], true).await?;
    let status = if error.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    let page = AdminRoundPage {
        layout: Layout::new(ctx, format!("Admin · {}", round.title)),
        block: RoundBlock { round, polls },
        post_slugs: posts::published_slugs(&state.db).await?,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

pub async fn close_round(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    votes::close_round_now(&state.db, id).await?;
    Ok(see_other(&format!("/admin/rounds/{id}")))
}

#[derive(Deserialize)]
pub struct HiddenForm {
    hidden: String,
}

pub async fn set_hidden(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(id): Path<i64>,
    Form(form): Form<HiddenForm>,
) -> AppResult<Response> {
    let round_id = votes::set_hidden(&state.db, id, form.hidden == "1").await?.ok_or(AppError::NotFound)?;
    Ok(see_other(&format!("/admin/rounds/{round_id}")))
}

#[derive(Deserialize)]
pub struct FulfillForm {
    #[serde(default)]
    post_slug: String,
    round_id: i64,
}

pub async fn fulfill(
    State(state): State<AppState>,
    ctx: Ctx,
    _admin: RequireAdmin,
    Path(id): Path<i64>,
    Form(form): Form<FulfillForm>,
) -> AppResult<Response> {
    let slug = form.post_slug.trim();
    match votes::set_fulfilled(&state.db, id, (!slug.is_empty()).then_some(slug)).await {
        Ok(round_id) => Ok(see_other(&format!("/admin/rounds/{round_id}"))),
        Err(VoteError::Database(e)) => Err(e.into()),
        Err(VoteError::NotFound) => Err(AppError::NotFound),
        Err(e) => round_page(&state, &ctx, form.round_id, Some(e.to_string())).await,
    }
}
