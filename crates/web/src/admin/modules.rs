//! 모듈 관리 화면 (ADR-0066): 켜기·끄기·제거·설정·실패 보관함. 권한은 서비스가 본다.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maru_core::{Code, Error};
use maru_modules::service;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{SignedIn, base, fail, merge, render, require, require_post};
use crate::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/modules", get(page))
        .route("/admin/modules/{id}/settings", post(save_settings))
        .route("/admin/modules/{id}/{action}", post(action))
}

#[derive(Debug, Default, Deserialize)]
struct DoneQuery {
    #[serde(default)]
    done: String,
}

fn notice(done: &str) -> Option<&'static str> {
    match done {
        "enable" => Some("모듈을 켰습니다."),
        "disable" => Some("모듈을 껐습니다. 데이터는 그대로입니다."),
        "remove" => Some("모듈을 제거하고 데이터를 지웠습니다."),
        "settings" => Some("설정을 저장했습니다."),
        _ => None,
    }
}

async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DoneQuery>,
) -> Response {
    let who = match require(&state, &headers, "/admin/modules").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    show(&state, &who, StatusCode::OK, notice(&q.done), None, None).await
}

/// 설정이 거절되면 그 모듈의 칸에 쓴 값을 그대로 다시 보인다.
async fn show(
    state: &AppState,
    who: &SignedIn,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<&str>,
    kept: Option<(&str, &HashMap<String, String>)>,
) -> Response {
    let list = match service::list(&state.ctx, &who.actor, &state.modules).await {
        Ok(l) => l,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let failures = match service::failures(&state.ctx, &who.actor, &state.modules, None).await {
        Ok(f) => f,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let items: Vec<Value> = list
        .iter()
        .map(|m| {
            let fields: Vec<Value> = m
                .settings_fields
                .iter()
                .map(|f| {
                    let typed = kept.filter(|(id, _)| *id == m.id).map(|(_, form)| form);
                    let value = match typed {
                        Some(form) => form.get(&f.key).cloned().unwrap_or_default(),
                        None => match m.settings.get(&f.key) {
                            Some(Value::String(s)) => s.clone(),
                            Some(Value::Null) | None => String::new(),
                            Some(v) => v.to_string(),
                        },
                    };
                    json!({ "key": f.key, "label": f.label, "kind": f.kind, "help": f.help, "value": value })
                })
                .collect();
            json!({
                "id": m.id,
                "name": m.name,
                "version": m.version,
                "description": m.description,
                "installed": m.installed,
                "enabled": m.enabled,
                "fields": fields,
                "tools": m.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
                "widgets": m.widgets,
                "admin_url": m.admin_url,
                "failures": m.failures,
            })
        })
        .collect();
    let ctx = merge(
        base(state, Some(who), "modules").await,
        json!({
            "notice": notice,
            "error": error,
            "items": items,
            "failures": failures,
        }),
    );
    render(state, status, "modules.html", &ctx)
}

#[derive(Debug, Default, Deserialize)]
struct ActionForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
    /// 제거 확인 칸. 모듈 id 를 그대로 적어야 한다
    #[serde(default)]
    confirm: String,
}

async fn action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, action)): Path<(String, String)>,
    Form(f): Form<ActionForm>,
) -> Response {
    let who = match require_post(&state, &headers, &f.csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let (ctx, actor, reg) = (&state.ctx, &who.actor, state.modules.as_ref());
    let res = match action.as_str() {
        "enable" => service::enable(ctx, actor, reg, &id).await.map(|_| ()),
        "disable" => service::disable(ctx, actor, reg, &id).await.map(|_| ()),
        "remove" if f.confirm.trim() == id => {
            service::remove(ctx, actor, reg, &id).await.map(|_| ())
        }
        "remove" => {
            return show(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some("제거하려면 확인 칸에 모듈 이름을 그대로 적으세요."),
                None,
            )
            .await;
        }
        _ => Err(Error::new(Code::NotFound)),
    };
    match res {
        Ok(()) => Redirect::to(&format!("/admin/modules?done={action}")).into_response(),
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}

async fn save_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(mut f): Form<HashMap<String, String>>,
) -> Response {
    let csrf = f.remove("_csrf").unwrap_or_default();
    let who = match require_post(&state, &headers, &csrf).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let patch: Map<String, Value> = f
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    match service::set_settings(&state.ctx, &who.actor, &state.modules, &id, patch).await {
        Ok(_) => Redirect::to("/admin/modules?done=settings").into_response(),
        Err(e) if e.code() == Code::Validation => {
            show(
                &state,
                &who,
                StatusCode::UNPROCESSABLE_ENTITY,
                None,
                Some(&e.message()),
                Some((&id, &f)),
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}
