//! 모듈을 웹에 붙이는 곳 (ADR-0066): 라우트를 `/m/<id>` 아래에 놓고, 모듈이 맡기는 일을 대신한다.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use maru_core::{Actor, Ctx, Error, Result};
use maru_modules::{COMMENT_ACTIONS, Host, ME_SECTIONS, ModuleState, POST_FOOTER, SlotView};
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
    };
    let slots = state
        .modules
        .slots(&state.ctx, &[POST_FOOTER, COMMENT_ACTIONS], &slot_view)
        .await;
    view["slots"] = Value::Object(slots);
}

/// 내 정보 화면의 자리.
pub(crate) async fn me_slots(state: &AppState, viewer: &Actor) -> Value {
    let slot_view = SlotView {
        viewer,
        post_id: None,
        comment_ids: &[],
    };
    Value::Object(
        state
            .modules
            .slots(&state.ctx, &[ME_SECTIONS], &slot_view)
            .await,
    )
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
