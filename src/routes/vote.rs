use std::time::Duration;

use axum::{
    Form,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::{OnForm, see_other};
use crate::{
    error::{AppError, AppResult},
    ip::{ClientIp, hash_ip, network_key},
    state::AppState,
    templates::{Ctx, Layout, PastRound, PollPartial, RoundBlock, RoundPage, VoteIndexPage, render},
    votes::{self, VoteError},
};

/// Global cap on suggestions per network across all polls, on top of the
/// per-poll limit, to slow down spam.
const SUGGEST_LIMIT: (u32, Duration) = (15, Duration::from_secs(60 * 60));

pub async fn index(State(state): State<AppState>, ctx: Ctx, ClientIp(ip): ClientIp) -> AppResult<Response> {
    let ip_hash = hash_ip(&state.config.ip_hash_secret, ip);
    let mut open = Vec::new();
    for round in votes::open_rounds(&state.db).await? {
        let polls = votes::polls_for_round(&state.db, &state.config, &round, &ip_hash, false).await?;
        open.push(RoundBlock { round, polls });
    }
    let past_rounds = votes::past_rounds(&state.db, 20).await?;
    let ids: Vec<i64> = past_rounds.iter().map(|r| r.id).collect();
    let mut winners = votes::winners_for(&state.db, &ids).await?;
    let past = past_rounds
        .into_iter()
        .map(|round| PastRound { winners: winners.remove(&round.id).unwrap_or_default(), round })
        .collect();

    render(&VoteIndexPage {
        layout: Layout::new(&ctx, "Vote for the next topics")
            .with_description("Suggest and vote on what the community should write about next. No account needed."),
        open,
        past,
        max_votes: state.config.max_votes_per_poll,
        max_suggestions: state.config.max_suggestions_per_poll,
    })
}

pub async fn round(
    State(state): State<AppState>,
    ctx: Ctx,
    ClientIp(ip): ClientIp,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let round = votes::get_round(&state.db, id).await?.ok_or(AppError::NotFound)?;
    let ip_hash = hash_ip(&state.config.ip_hash_secret, ip);
    let polls = votes::polls_for_round(&state.db, &state.config, &round, &ip_hash, false).await?;
    render(&RoundPage { layout: Layout::new(&ctx, round.title.clone()), block: RoundBlock { round, polls } })
}

#[derive(Deserialize)]
pub struct SuggestForm {
    title: String,
    #[serde(default)]
    details: String,
}

pub async fn suggest(
    State(state): State<AppState>,
    ctx: Ctx,
    ClientIp(ip): ClientIp,
    Path(poll_id): Path<i64>,
    Form(form): Form<SuggestForm>,
) -> AppResult<Response> {
    let ip_hash = hash_ip(&state.config.ip_hash_secret, ip);
    let result = if state.limiter.check(&format!("suggest:{}", network_key(ip)), SUGGEST_LIMIT.0, SUGGEST_LIMIT.1) {
        votes::submit_suggestion(&state.db, &state.config, poll_id, &ip_hash, ctx.user_id(), &form.title, &form.details)
            .await
    } else {
        Err(VoteError::Invalid(AppError::TooManyRequests.to_string()))
    };
    respond_with_poll(&state, &ctx, poll_id, &ip_hash, result.map(|_| "Thanks! Your suggestion is in.")).await
}

pub async fn vote(
    State(state): State<AppState>,
    ctx: Ctx,
    ClientIp(ip): ClientIp,
    Path(suggestion_id): Path<i64>,
    Form(form): Form<OnForm>,
) -> AppResult<Response> {
    let ip_hash = hash_ip(&state.config.ip_hash_secret, ip);
    let poll_id: i64 = sqlx::query_scalar("SELECT poll_id FROM suggestions WHERE id = $1")
        .bind(suggestion_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let result = votes::set_vote(&state.db, &state.config, suggestion_id, &ip_hash, form.on()).await;
    respond_with_poll(&state, &ctx, poll_id, &ip_hash, result.map(|_| "")).await
}

/// Re-render the whole poll after a change: the vote counter, ordering and
/// "votes left" all depend on each other, so a partial update would drift.
async fn respond_with_poll(
    state: &AppState,
    ctx: &Ctx,
    poll_id: i64,
    ip_hash: &[u8],
    result: Result<&str, VoteError>,
) -> AppResult<Response> {
    let mut pv = votes::poll_view(&state.db, &state.config, poll_id, ip_hash).await?.ok_or(AppError::NotFound)?;
    match result {
        Ok(notice) if !notice.is_empty() => pv.notice = Some(notice.to_string()),
        Ok(_) => {}
        Err(VoteError::NotFound) => return Err(AppError::NotFound),
        Err(VoteError::Database(e)) => return Err(e.into()),
        Err(e) => pv.error = Some(e.to_string()),
    }
    if ctx.htmx {
        return render(&PollPartial { pv });
    }
    let Some(error) = pv.error.take() else {
        // Success without JavaScript: Post/Redirect/Get back to the round.
        return Ok(see_other(&format!("/vote/rounds/{}#poll-{}", pv.poll.round_id, pv.poll.id)));
    };
    // Failure without JavaScript: a redirect would lose the message, so render
    // the round page directly with the error shown on the poll it concerns.
    let round = votes::get_round(&state.db, pv.poll.round_id).await?.ok_or(AppError::NotFound)?;
    let mut polls = votes::polls_for_round(&state.db, &state.config, &round, ip_hash, false).await?;
    if let Some(p) = polls.iter_mut().find(|p| p.poll.id == pv.poll.id) {
        p.error = Some(error);
    }
    let page = RoundPage { layout: Layout::new(ctx, round.title.clone()), block: RoundBlock { round, polls } };
    Ok((StatusCode::UNPROCESSABLE_ENTITY, render(&page)?).into_response())
}
