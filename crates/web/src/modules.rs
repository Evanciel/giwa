//! 모듈을 웹에 붙이는 곳 (ADR-0066): 라우트를 `/m/<id>` 아래에 놓고, 모듈이 맡기는 일을 대신한다.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use maru_core::{Actor, Ctx, Error, Result};
use maru_modules::{
    ACCOUNT_BAR, COMMENT_ACTIONS, Host, ME_SECTIONS, MEMBER_BADGE, ModuleState, POST_FOOTER,
    POST_LIST, SlotView,
};
use serde_json::{Value, json};

use crate::AppState;

struct WebHost(AppState);

#[async_trait]
impl Host for WebHost {
    async fn page(&self, template: &str, title: &str, data: Value) -> Response {
        let site = crate::public::site_json(&self.0).await;
        let name = site["name"].as_str().unwrap_or("Maru");
        let noindex = data["noindex"].as_bool().unwrap_or(false);
        let mut ctx = json!({
            "site": site,
            "meta": { "title": format!("{title} · {name}"), "noindex": noindex },
        });
        if let (Some(c), Value::Object(d)) = (ctx.as_object_mut(), data) {
            c.extend(d);
        }
        crate::public::render(&self.0, template, Ok(ctx)).await
    }

    async fn error(&self, err: &Error) -> Response {
        crate::error::page(&self.0, err).await
    }

    async fn viewer(&self, headers: &HeaderMap) -> Result<Actor> {
        let (actor, _) = crate::comments::viewer(&self.0, headers).await?;
        Ok(actor)
    }

    fn same_origin(&self, headers: &HeaderMap) -> bool {
        crate::comments::same_origin(headers)
    }
}

/// 글 화면의 자리를 채운다. 켜진 모듈이 없으면 누구인지 묻지 않는다.
pub(crate) async fn add_post_slots(state: &AppState, headers: &HeaderMap, view: &mut Value) {
    if state.modules.enabled(&state.ctx).await.is_empty() {
        return;
    }
    let Some(post_id) = view["post"]["id"].as_str().map(str::to_owned) else {
        return;
    };
    let comment_ids: Vec<String> = view["comments"]["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|c| c["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let viewer = match crate::comments::viewer(state, headers).await {
        Ok((actor, _)) => actor,
        Err(_) => Actor::anonymous([]),
    };
    let slot_view = SlotView {
        viewer: &viewer,
        post_id: Some(&post_id),
        comment_ids: &comment_ids,
        post_ids: &[],
        member_ids: &[],
    };
    let slots = state
        .modules
        .slots(&state.ctx, &[POST_FOOTER, COMMENT_ACTIONS], &slot_view)
        .await;
    view["slots"] = Value::Object(slots);
}

/// 게시판 목록의 자리 (ADR-0082). 공개 목록 HTML 은 모두에게 같아 손님으로 묻는다.
pub(crate) async fn add_list_slots(state: &AppState, view: &mut Value) {
    if state.modules.enabled(&state.ctx).await.is_empty() {
        return;
    }
    let post_ids: Vec<String> = ["notices", "posts"]
        .iter()
        .filter_map(|k| view[*k].as_array())
        .flatten()
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect();
    if post_ids.is_empty() {
        return;
    }
    let viewer = Actor::anonymous([]);
    let slot_view = SlotView {
        viewer: &viewer,
        post_id: None,
        comment_ids: &[],
        post_ids: &post_ids,
        member_ids: &[],
    };
    let slots = state
        .modules
        .slots(&state.ctx, &[POST_LIST], &slot_view)
        .await;
    // 표 목록 왼쪽 추천 수 칸 (GAP Q29, ADR-0112): 글마다 수를 주는 첫 모듈의 것. 없는 글은 0.
    if let Some(v) = slots
        .get(POST_LIST)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s["data"]["by_id"].is_object())
    {
        view["vote_counts"] = json!({ "label": v["data"]["label"], "by_id": v["data"]["by_id"] });
    }
    view["slots"] = Value::Object(slots);
}

/// 회원 이름 옆 표시 (GAP Q1, ADR-0086). 화면 값에서 `member_id` 를 가진 칸(글쓴이·댓글)을 찾아
/// 모듈이 준 `{label, title}` 를 `badge` 로 붙인다. 보는 사람과 무관하게 손님으로 묻는다(목록 캐시).
pub(crate) async fn add_member_badges(state: &AppState, view: &mut Value) {
    let mut ids = Vec::new();
    collect_member_ids(view, &mut ids);
    if ids.is_empty() {
        return;
    }
    let mut badges = staff_badges(state, &ids).await;
    if state.modules.enabled(&state.ctx).await.is_empty() {
        if !badges.is_empty() {
            attach_badges(view, &badges);
        }
        return;
    }
    let viewer = Actor::anonymous([]);
    let slot_view = SlotView {
        viewer: &viewer,
        post_id: None,
        comment_ids: &[],
        post_ids: &[],
        member_ids: &ids,
    };
    let slots = state
        .modules
        .slots(&state.ctx, &[MEMBER_BADGE], &slot_view)
        .await;
    // 모듈이 여럿이면 먼저 켠 모듈의 표시를 쓴다. 운영진 표시가 있으면 그것이 먼저다.
    for item in slots
        .get(MEMBER_BADGE)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(map) = item["data"]["by_member"].as_object() {
            for (id, b) in map {
                badges.entry(id.clone()).or_insert_with(|| b.clone());
            }
        }
    }
    if !badges.is_empty() {
        attach_badges(view, &badges);
    }
}

/// 운영진 표시 (GAP Q25, ADR-0107): 편집자 이상 역할을 가진 회원. 모듈과 무관하게 코어 역할로 정한다.
async fn staff_badges(state: &AppState, ids: &[String]) -> serde_json::Map<String, Value> {
    const EDITOR_RANK: i64 = 20;
    const ADMIN_RANK: i64 = 30;
    let mut out = serde_json::Map::new();
    for id in ids {
        let Ok(roles) = state.ctx.store.member_roles(id).await else {
            continue;
        };
        let rank = roles.iter().map(|r| r.rank).max().unwrap_or(0);
        let label = if rank >= ADMIN_RANK {
            "관리자"
        } else if rank >= EDITOR_RANK {
            "운영진"
        } else {
            continue;
        };
        out.insert(
            id.clone(),
            json!({ "label": label, "title": label, "staff": true }),
        );
    }
    out
}

fn collect_member_ids(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(id) = map.get("member_id").and_then(Value::as_str)
                && !out.iter().any(|x| x == id)
            {
                out.push(id.to_owned());
            }
            for child in map.values() {
                collect_member_ids(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_member_ids(i, out)),
        _ => {}
    }
}

fn attach_badges(v: &mut Value, badges: &serde_json::Map<String, Value>) {
    match v {
        Value::Object(map) => {
            let badge = map
                .get("member_id")
                .and_then(Value::as_str)
                .and_then(|id| badges.get(id))
                .cloned();
            if let Some(b) = badge {
                map.insert("badge".to_owned(), b);
            }
            for child in map.values_mut() {
                attach_badges(child, badges);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| attach_badges(i, badges)),
        _ => {}
    }
}

/// 내 정보 화면의 자리.
pub(crate) async fn me_slots(state: &AppState, viewer: &Actor) -> Value {
    let slot_view = SlotView {
        viewer,
        post_id: None,
        comment_ids: &[],
        post_ids: &[],
        member_ids: &[],
    };
    Value::Object(
        state
            .modules
            .slots(&state.ctx, &[ME_SECTIONS], &slot_view)
            .await,
    )
}

/// 회원 막대에 모듈이 넣는 항목 (ADR-0081).
pub(crate) async fn bar_slots(state: &AppState, viewer: &Actor) -> Vec<Value> {
    let slot_view = SlotView {
        viewer,
        post_id: None,
        comment_ids: &[],
        post_ids: &[],
        member_ids: &[],
    };
    let mut map = state
        .modules
        .slots(&state.ctx, &[ACCOUNT_BAR], &slot_view)
        .await;
    match map.remove(ACCOUNT_BAR) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

#[derive(Clone)]
struct Gate {
    ctx: Ctx,
    id: &'static str,
}

/// 꺼진 모듈의 주소는 없는 주소다. CLI 로 꺼도 재시작이 필요 없게 요청마다 DB 를 읽는다.
async fn enabled_only(State(gate): State<Gate>, req: Request, next: Next) -> Response {
    match gate.ctx.store.module_row(gate.id).await {
        Ok(Some(row)) if row.enabled => next.run(req).await,
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(module = gate.id, error = ?e, "모듈 상태를 못 읽었습니다");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// 모든 모듈의 라우트. 시작할 때 다 붙여 두고, 켜짐은 요청마다 본다.
pub(crate) fn routes(state: &AppState) -> Router<AppState> {
    let module_state = ModuleState {
        ctx: state.ctx.clone(),
        host: Arc::new(WebHost(state.clone())),
    };
    let mut app = Router::new();
    for m in state.modules.iter() {
        let Some(routes) = m.routes() else {
            continue;
        };
        let id = m.manifest().id;
        let gate = Gate {
            ctx: state.ctx.clone(),
            id,
        };
        app = app.nest(
            &format!("/m/{id}"),
            routes
                .with_state(module_state.clone())
                .layer(middleware::from_fn_with_state(gate, enabled_only)),
        );
    }
    app
}
