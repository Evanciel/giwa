//! 변경 묶음 화면 (SPEC §6, ADR-0123): 에이전트가 담은 변경을 사람이 보고 적용·버리기·되돌리기.
//! 권한은 서비스가 검사한다.

use axum::Router;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maru_core::actor::Permission;
use maru_core::clock;
use maru_core::model::{Changeset, ChangesetStatus};
use maru_core::services::changesets;
use maru_core::{Code, Error};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{SignedIn, base, fail, merge, render, require, require_post, site_tz};
use crate::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/changesets", get(list_page))
        .route("/admin/changesets/{id}", get(detail_page))
        .route("/admin/changesets/{id}/{action}", post(action))
}

fn status_name(status: ChangesetStatus) -> &'static str {
    match status {
        ChangesetStatus::Preview => "승인 대기",
        ChangesetStatus::Applying => "적용 도중 멈춤",
        ChangesetStatus::Applied => "적용함",
        ChangesetStatus::Failed => "되돌리다 멈춤",
        ChangesetStatus::Discarded => "버림",
        ChangesetStatus::Reverted => "되돌림",
    }
}

fn who_name(cs: &Changeset) -> &'static str {
    match cs.actor_type.as_str() {
        "agent" => "에이전트",
        "human" => "회원",
        _ => "이 컴퓨터",
    }
}

/// 연산 하나를 사람이 읽는 말로. 값은 사용자·에이전트가 넣은 데이터다.
fn op_view(op: &Value) -> Value {
    let name = op["op"].as_str().unwrap_or_default();
    let label = match name {
        "board.create" => "게시판 만들기",
        "board.update" => "게시판 고치기",
        "board.delete" => "게시판 지우기",
        "page.create" => "페이지 만들기",
        "page.update" => "페이지 고치기",
        "page.publish" => "페이지 발행",
        "page.hide" => "페이지 숨기기",
        "page.delete" => "페이지 지우기",
        "page.restore" => "페이지 되살리기",
        "menu.set" => "메뉴 바꾸기",
        "tokens.set" => "디자인 토큰 바꾸기",
        "tokens.reset" => "디자인 토큰 초기화",
        "settings.set" => "사이트 설정 바꾸기",
        "widget.place" => "위젯 놓기",
        "widget.configure" => "위젯 고치기",
        "widget.remove" => "위젯 빼기",
        "module.enable" => "모듈 켜기",
        "module.disable" => "모듈 끄기",
        "preset.step" => return step_view(op),
        _ => name,
    };
    let target = match name {
        // 위젯 id 대신 무엇을 놓고 무엇을 고쳤는지 (화면 편집기 묶음, ADR-0138).
        "widget.place" => {
            maru_core::services::widgets::kind_name(op["kind"].as_str().unwrap_or_default())
                .to_owned()
        }
        "widget.configure" => widget_patch_text(&op["patch"]),
        _ => ["slug", "location", "id"]
            .iter()
            .find_map(|k| op[*k].as_str())
            .unwrap_or_default()
            .to_owned(),
    };
    json!({
        "label": label,
        "target": target,
        "json": serde_json::to_string_pretty(op).unwrap_or_default(),
    })
}

/// 위젯 고치기에서 바뀐 칸을 사람 말로: 「순서 · 숨기기」.
fn widget_patch_text(patch: &Value) -> String {
    let Some(p) = patch.as_object() else {
        return String::new();
    };
    let mut out = Vec::new();
    for (key, word) in [
        ("region", "자리 옮기기"),
        ("position", "순서"),
        ("scope", "보이는 곳"),
        ("title", "제목"),
        ("config", "설정"),
    ] {
        if p.contains_key(key) {
            out.push(word);
        }
    }
    match p.get("enabled").and_then(Value::as_bool) {
        Some(true) => out.push("다시 보이기"),
        Some(false) => out.push("숨기기"),
        None => {}
    }
    out.join(" · ")
}

/// 테마(프리셋)의 한 단계. 모두 「없으면 만든다」라서 이미 있는 것은 건드리지 않는다.
fn step_view(op: &Value) -> Value {
    let step = &op["step"];
    let name = |k: &str| step[k].as_str().unwrap_or_default().to_owned();
    let (label, target) = match step["op"].as_str().unwrap_or_default() {
        "board.ensure" => ("게시판 갖추기", name("name")),
        "page.ensure" => ("페이지 갖추기", name("title")),
        "module.ensure" => ("기능 켜기", name("id")),
        "menu.ensure" => ("메뉴가 비어 있으면 채우기", String::new()),
        "tokens.ensure" => ("스킨(색·모양) 입히기, 기본 색일 때만", String::new()),
        "settings.ensure" => ("기본값인 설정 채우기", String::new()),
        "widgets.ensure" => (
            "화면 조각 놓기, 그 자리가 비어 있을 때만",
            step["items"]
                .as_array()
                .map(|a| format!("{}개", a.len()))
                .unwrap_or_default(),
        ),
        other => (other, String::new()),
    };
    json!({
        "label": label,
        "target": target,
        "json": serde_json::to_string_pretty(op).unwrap_or_default(),
    })
}

fn view(cs: &Changeset, tz: i32) -> Value {
    let date = |at: &str| clock::display_datetime(at, tz);
    json!({
        "id": cs.id,
        "title": cs.title,
        "status": cs.status.as_str(),
        "status_name": status_name(cs.status),
        "who": who_name(cs),
        "error": cs.error,
        "created_at": date(&cs.created_at),
        "applied_at": cs.applied_at.as_deref().map(date),
        "reverted_at": cs.reverted_at.as_deref().map(date),
        "op_count": cs.ops.len(),
        "ops": cs.ops.iter().map(op_view).collect::<Vec<_>>(),
        "can_preview": cs.status == ChangesetStatus::Preview,
        "can_apply": cs.status == ChangesetStatus::Preview && !cs.ops.is_empty(),
        "can_discard": cs.status == ChangesetStatus::Preview,
        "can_revert": matches!(
            cs.status,
            ChangesetStatus::Applied | ChangesetStatus::Applying | ChangesetStatus::Failed
        ),
    })
}

#[derive(Debug, Default, Deserialize)]
struct DoneQuery {
    #[serde(default)]
    done: String,
}

fn notice(done: &str) -> Option<&'static str> {
    match done {
        "apply" => Some("사이트에 적용했습니다. 문제가 있으면 되돌리기 한 번으로 돌아갑니다."),
        "discard" => Some("버렸습니다. 사이트는 그대로입니다."),
        "revert" => Some("되돌렸습니다."),
        "theme" => Some(
            "아직 사이트는 그대로입니다. 미리 보기로 확인하고 「사이트에 적용」을 누르세요. 적용한 뒤에도 되돌리기 한 번으로 돌아갑니다.",
        ),
        _ => None,
    }
}

async fn list_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let who = match require(&state, &headers, "/admin/changesets").await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let list = match changesets::list(&state.ctx, &who.actor, None, 100).await {
        Ok(l) => l,
        Err(e) => return fail(&state, Some(&who), &e).await,
    };
    let tz = site_tz(&state).await;
    let items: Vec<Value> = list.iter().map(|cs| view(cs, tz)).collect();
    let ctx = merge(
        base(&state, Some(&who), "changesets").await,
        json!({ "items": items }),
    );
    render(&state, StatusCode::OK, "changesets.html", &ctx)
}

async fn detail(
    state: &AppState,
    who: &SignedIn,
    id: &str,
    status: StatusCode,
    notice: Option<&str>,
    error: Option<&str>,
) -> Response {
    let cs = match changesets::get(&state.ctx, &who.actor, id).await {
        Ok(cs) => cs,
        Err(e) => return fail(state, Some(who), &e).await,
    };
    let ctx = merge(
        base(state, Some(who), "changesets").await,
        json!({
            "notice": notice,
            "error": error,
            "cs": view(&cs, site_tz(state).await),
            "approver": who.actor.has(Permission::SiteAdmin),
        }),
    );
    render(state, status, "changeset.html", &ctx)
}

async fn detail_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Response {
    let back = format!("/admin/changesets/{id}");
    let who = match require(&state, &headers, &back).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    detail(&state, &who, &id, StatusCode::OK, notice(&q.done), None).await
}

#[derive(Debug, Default, Deserialize)]
struct ActionForm {
    #[serde(default, rename = "_csrf")]
    csrf: String,
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
    let (ctx, actor) = (&state.ctx, &who.actor);
    let res = match action.as_str() {
        "apply" => match state.sandbox() {
            Ok(sandbox) => changesets::apply(ctx, actor, sandbox.as_ref(), &id).await,
            Err(e) => Err(e),
        },
        "discard" => changesets::discard(ctx, actor, &id).await,
        "revert" => changesets::revert(ctx, actor, &id).await,
        _ => Err(Error::new(Code::NotFound)
            .with_message("error.changeset_not_found")
            .arg("id", id.as_str())),
    };
    state.close_preview(&id).await;
    match res {
        Ok(_) => Redirect::to(&format!("/admin/changesets/{id}?done={action}")).into_response(),
        // 멈춘 까닭은 묶음 화면에서 보인다.
        Err(e) if matches!(e.code(), Code::Conflict | Code::Validation | Code::NotFound) => {
            let message = e.message();
            detail(
                &state,
                &who,
                &id,
                StatusCode::CONFLICT,
                None,
                Some(&message),
            )
            .await
        }
        Err(e) => fail(&state, Some(&who), &e).await,
    }
}
