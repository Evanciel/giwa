//! 관리 화면 첫 쪽(대시보드). 처음 온 운영자가 할 일과 사이트 현황을 한눈에 보여준다.
//! 값은 모두 같은 서비스 레이어에서 읽고, 권한이 없는 칸은 비운다.

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use maru_core::actor::Permission;
use maru_core::model::{CommentStatus, PageReq, PostStatus, SiteSettings};
use maru_core::services::auth::SignedIn;
use maru_core::services::{
    boards, comments, design, members, posts, samples, settings, signals, stats,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CsrfForm, base, fail, merge, notice, post_row, render, require, require_post};
use crate::AppState;

#[derive(Debug, Default, Deserialize)]
pub(super) struct DashQuery {
    #[serde(default)]
    done: String,
}

/// 방문 그래프 칸 수 (오늘 포함)
const DAYS: u32 = 14;
/// 그래프 SVG 의 막대 영역
const CHART_W: u32 = 560;
const CHART_H: u32 = 140;

pub(super) async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let site = settings::site(&state.ctx).await.unwrap_or_default();
    let published = post_count(&state, &who, Some(PostStatus::Published)).await;
    let drafts = post_count(&state, &who, Some(PostStatus::Draft)).await;
    let moderator = who.actor.has(Permission::PostModerate);
    let sample = samples::status(&state.ctx, &who.actor).await.ok();
    let untouched = sample.as_ref().map_or(0, |s| s.untouched) as i64;

    let steps = [
        step(
            who.member.has_password(),
            "비밀번호 정하기",
            "로그인 링크 없이 이메일과 비밀번호로 들어올 수 있습니다.",
            "/admin/password",
            "정하기",
        ),
        step(
            site.name != SiteSettings::default().name,
            "사이트 이름 정하기",
            "머리글·검색 결과·공유 카드에 나오는 이름입니다.",
            "/admin/settings",
            "이름 정하기",
        ),
        step(
            !site.description.trim().is_empty(),
            "한 줄 소개 쓰기",
            "검색 엔진과 공유 미리보기에 쓰입니다.",
            "/admin/settings",
            "소개 쓰기",
        ),
        step(
            published > untouched,
            "첫 글 발행하기",
            "게시판을 고르고 글을 쓰면 첫 화면에 바로 올라갑니다.",
            "/admin/posts/new",
            "글쓰기",
        ),
    ];
    let done_steps = steps.iter().filter(|s| s["done"] == true).count();

    let ctx = merge(
        base(&state, Some(&who), "dashboard").await,
        json!({
            "notice": notice(&q.done),
            "site_url": state.ctx.site_url,
            "steps": steps,
            "done_steps": done_steps,
            "steps_pct": done_steps * 100 / steps.len(),
            "signals": if moderator {
                signals::list(&state.ctx, &who.actor).await.unwrap_or_default()
            } else {
                Vec::new()
            },
            "published": published,
            "drafts": drafts,
            "pending": if moderator { Some(comment_total(&state, &who, Some(CommentStatus::Pending)).await) } else { None },
            "members": members::list(&state.ctx, &who.actor, PageReq::new(1, 1)).await.ok().map(|l| l.total),
            "visits": visits(&state, &who).await,
            "recent_posts": recent_posts(&state, &who, site.tz_offset_min).await,
            "recent_comments": if moderator { recent_comments(&state, &who).await } else { Vec::new() },
            "moderator": moderator,
            "theme": current_theme(&state).await,
            "sample": sample,
        }),
    );
    render(&state, StatusCode::OK, "dashboard.html", &ctx)
}

/// 고치지 않은 예시 글을 한 번에 지운다 (ADR-0142). 폼의 확인 칸은 브라우저가 묻는다.
pub(super) async fn clear_samples(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    match samples::clear(&state.ctx, &who.actor).await {
        Ok(_) => Redirect::to("/admin?done=samples").into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

fn step(done: bool, title: &str, hint: &str, href: &str, cta: &str) -> Value {
    json!({ "done": done, "title": title, "hint": hint, "href": href, "cta": cta })
}

async fn post_count(state: &AppState, who: &SignedIn, status: Option<PostStatus>) -> i64 {
    posts::list(
        &state.ctx,
        &who.actor,
        None,
        status,
        None,
        PageReq::new(1, 1),
    )
    .await
    .map(|l| l.total)
    .unwrap_or(0)
}

async fn comment_total(state: &AppState, who: &SignedIn, status: Option<CommentStatus>) -> i64 {
    comments::admin_list(&state.ctx, &who.actor, status, None, PageReq::new(1, 1))
        .await
        .map(|l| l.total)
        .unwrap_or(0)
}

async fn recent_posts(state: &AppState, who: &SignedIn, tz: i32) -> Vec<Value> {
    let Ok(list) = posts::list(&state.ctx, &who.actor, None, None, None, PageReq::new(1, 6)).await
    else {
        return Vec::new();
    };
    let boards = boards::list(&state.ctx, &who.actor)
        .await
        .unwrap_or_default();
    list.items
        .iter()
        .map(|p| {
            let b = boards.iter().find(|b| b.id == p.board_id);
            let mut row = post_row(
                p,
                b.map(|b| b.slug.as_str()).unwrap_or_default(),
                b.map(|b| b.name.as_str()).unwrap_or_default(),
                tz,
            );
            row["views"] = json!(state.views.shown(p));
            row
        })
        .collect()
}

/// 최근 댓글. 본문은 사용자 입력이라 줄여서 글자로만 보인다.
async fn recent_comments(state: &AppState, who: &SignedIn) -> Vec<Value> {
    let Ok(list) =
        comments::admin_list(&state.ctx, &who.actor, None, None, PageReq::new(1, 5)).await
    else {
        return Vec::new();
    };
    list.items
        .iter()
        .map(|c| {
            let excerpt: String = c.body.chars().take(70).collect();
            let more = c.body.chars().count() > 70;
            json!({
                "author": c.author_name,
                "guest": c.is_guest,
                "excerpt": if more { format!("{excerpt}…") } else { excerpt },
                "status": c.status.as_str(),
                "status_label": match c.status {
                    CommentStatus::Published => "공개",
                    CommentStatus::Pending => "승인 대기",
                    CommentStatus::Hidden => "숨김",
                    CommentStatus::Spam => "스팸",
                    CommentStatus::Deleted => "삭제됨",
                },
                "day": c.created_at.get(..10).unwrap_or(&c.created_at),
            })
        })
        .collect()
}

/// 최근 14일 방문 그래프. 막대는 SVG 속성으로 그린다 (관리 화면 CSP 는 인라인 style 을 막는다).
async fn visits(state: &AppState, who: &SignedIn) -> Option<Value> {
    let s = stats::summary(
        &state.ctx,
        &who.actor,
        &stats::StatsQuery { days: Some(DAYS) },
    )
    .await
    .ok()?;
    let n = s.daily.len().max(1) as u32;
    let slot = CHART_W / n;
    let max = s.daily.iter().map(|d| d.visitors).max().unwrap_or(0).max(1);
    let last = s.daily.len().saturating_sub(1);
    let bars: Vec<Value> = s
        .daily
        .iter()
        .enumerate()
        .map(|(i, d)| {
            // 0 인 날도 바닥에 얇게 남겨 날짜 칸이 보이게 한다.
            let h = ((d.visitors as f64 / max as f64) * f64::from(CHART_H)).round() as u32;
            let h = h.max(2);
            let md = d.day.get(5..).unwrap_or(&d.day).replace('-', ".");
            json!({
                "x": i as u32 * slot + slot / 6,
                "w": slot - slot / 3,
                "y": CHART_H - h,
                "h": h,
                "label": md,
                "show_label": i % 2 == last % 2,
                "visitors": d.visitors,
                "views": d.views,
                "today": i == last,
            })
        })
        .collect();
    let today = s.daily.last();
    let yesterday = s.daily.iter().rev().nth(1);
    let week: i64 = s.daily.iter().rev().take(7).map(|d| d.visitors).sum();
    Some(json!({
        "bars": bars,
        "width": CHART_W,
        "height": CHART_H,
        "max": max,
        "today_visitors": today.map_or(0, |d| d.visitors),
        "today_views": today.map_or(0, |d| d.views),
        "yesterday_visitors": yesterday.map_or(0, |d| d.visitors),
        "week_visitors": week,
        "total_visitors": s.visitors,
        "total_views": s.views,
    }))
}

/// 지금 입힌 테마. 어느 테마와도 같지 않으면 직접 고른 색으로 보여준다.
async fn current_theme(state: &AppState) -> Value {
    let tokens = design::tokens(&state.ctx).await.unwrap_or_default();
    match super::site::themes().iter().find(|t| t.is_current(&tokens)) {
        Some(t) => t.card(&tokens),
        None => json!({
            "id": "",
            "name": "직접 고른 색",
            "description": "디자인 화면에서 색을 따로 고쳤습니다.",
            "c": tokens.color,
            "d": tokens.color_dark,
            "rs": tokens.radius.sm.min(8),
            "rm": (tokens.radius.md / 2).min(10),
            "serif": tokens.font.heading == "serif",
        }),
    }
}
