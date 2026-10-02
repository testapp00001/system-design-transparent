use axum::{
    Form,
    extract::{Path, Query, State},
    response::Response,
};

use super::{OnForm, hx_redirect, see_other};
use crate::{
    error::{AppError, AppResult},
    posts::{self, ListFilter, ListParams, PAGE_SIZE, Reaction},
    state::AppState,
    templates::{
        CardView, Ctx, FilterView, HomePage, Layout, PostPage, PostView, ReactionsPartial, ReactionsView, Results,
        ResultsPartial, SortSelect, format_date, login_href, render,
    },
    votes,
};

/// Home page = searchable, filterable list of posts. When htmx asks (typing in
/// the search box, changing a filter) we return only the results fragment.
pub async fn index(State(state): State<AppState>, ctx: Ctx, Query(params): Query<ListParams>) -> AppResult<Response> {
    let filter = ListFilter::from_params(&params);
    let cards = posts::list(&state.db, &filter).await?;

    // A stale bookmark or an old page link beyond the last page: send the
    // visitor to the last real page instead of showing "0 articles".
    if cards.is_empty() && filter.page > 1 {
        let total = posts::count(&state.db, &filter).await?;
        let last = ((total + PAGE_SIZE - 1) / PAGE_SIZE).max(1);
        if last < filter.page {
            let to = format!("/?{}", filter.page_query(last));
            return Ok(if ctx.htmx { hx_redirect(&to) } else { see_other(&to) });
        }
    }
    let results = Results::new(cards, &filter, PAGE_SIZE);

    if ctx.htmx {
        return render(&ResultsPartial { results, sort: SortSelect::new(&filter, true) });
    }

    let show_hero = filter.q.is_empty() && filter.tag.is_none() && filter.level.is_none() && filter.page == 1;
    let title = match (&filter.tag, filter.q.is_empty()) {
        (Some(tag), _) => format!("#{tag}"),
        (None, false) => format!("Search: {}", filter.q),
        _ => crate::templates::SITE_NAME.to_string(),
    };
    render(&HomePage {
        layout: Layout::new(&ctx, title),
        filter: FilterView::new(&filter),
        sort: SortSelect::new(&filter, false),
        results,
        tags: posts::tags_with_counts(&state.db).await?,
        open_rounds: votes::open_rounds(&state.db).await?,
        show_hero,
    })
}

pub async fn show(State(state): State<AppState>, ctx: Ctx, Path(slug): Path<String>) -> AppResult<Response> {
    let post = posts::get_by_slug(&state.db, &slug).await?.ok_or(AppError::NotFound)?;
    let reaction_state = posts::reaction_state(&state.db, ctx.user_id(), post.id).await?;
    let related = posts::related(&state.db, &post).await?;
    let edit_url = format!("{}/edit/main/content/posts/{}.md", state.config.repo_url, post.slug);

    let layout = Layout::new(&ctx, post.title.clone()).with_description(post.summary.clone());
    let view = PostView {
        published: format_date(&post.published_at),
        updated: (post.updated_at.date_naive() != post.published_at.date_naive())
            .then(|| format_date(&post.updated_at)),
        slug: post.slug,
        title: post.title,
        summary: post.summary,
        level: post.level,
        tags: post.tags,
        body_html: post.body_html,
        toc_html: post.toc_html,
        reading_minutes: post.reading_minutes,
    };
    render(&PostPage {
        reactions: ReactionsView {
            slug: view.slug.clone(),
            logged_in: ctx.user.is_some(),
            state: reaction_state,
            login_href: login_href(&ctx.path),
        },
        layout,
        post: view,
        related: related.into_iter().map(CardView::from).collect(),
        edit_url,
    })
}

pub async fn react(
    State(state): State<AppState>,
    ctx: Ctx,
    Path((slug, kind)): Path<(String, String)>,
    Form(form): Form<OnForm>,
) -> AppResult<Response> {
    let post_path = format!("/posts/{slug}");
    let Some(user) = &ctx.user else {
        let to = login_href(&post_path);
        return Ok(if ctx.htmx { hx_redirect(&to) } else { see_other(&to) });
    };
    let kind = Reaction::parse(&kind).ok_or(AppError::NotFound)?;
    let post = posts::get_by_slug(&state.db, &slug).await?.ok_or(AppError::NotFound)?;
    posts::set_reaction(&state.db, user.id, post.id, kind, form.on()).await?;

    if !ctx.htmx {
        return Ok(see_other(&post_path));
    }
    render(&ReactionsPartial {
        reactions: ReactionsView {
            slug: post.slug,
            logged_in: true,
            state: posts::reaction_state(&state.db, Some(user.id), post.id).await?,
            login_href: login_href(&post_path),
        },
    })
}
