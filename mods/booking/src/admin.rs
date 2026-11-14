//! 예약 관리 화면 `/m/booking` (ADR-0169). 날짜별 예약·확정·취소, 서비스, 영업 시간, 막은 시간.
//!
//! 툴과 같은 함수를 부른다(원칙 1). 보기·설정·확정은 모듈 관리 권한, 취소는 그 안에서 사람만 받는다.
//! 손님 정보는 바깥 사람이 쓴 값이라 이스케이프해 그린다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use giwa_core::actor::{Actor, Permission};
use giwa_core::services::settings;
use giwa_core::{Code, Ctx, Error, Result, urls};
use giwa_modules::{ModuleState, module_error};
use serde::Deserialize;
use serde_json::{Value, json};

use super::web::no_store;
use super::{
    BlockInput, BookingModule, BookingRow, CONFIRMED, HELD, HOME, HoursInput, REQUESTED,
    ServiceRow, ServiceSetInput, WEEKDAY_KEYS, day_label, day_str, hours_view, local_now,
    parse_day,
};

#[derive(Debug, Default, Deserialize)]
pub struct AdminQuery {
    #[serde(default)]
    day: String,
    #[serde(default)]
    done: String,
}

fn done_text(done: &str) -> Option<&'static str> {
    Some(match done {
        "confirm" => "예약을 확정하고 손님에게 편지를 보냈습니다.",
        "cancel" => "예약을 취소하고 손님에게 알렸습니다.",
        "refund" => "예약을 취소했습니다. 받은 예약금은 결제 관리에서 환불하세요.",
        "service" => "서비스를 저장했습니다.",
        "hours" => "영업 시간을 저장했습니다.",
        "block" => "그 시간을 막았습니다.",
        "unblock" => "막은 시간을 지웠습니다.",
        _ => return None,
    })
}

/// 로그인한 운영자. 손님이면 로그인 화면으로.
async fn operator(
    state: &ModuleState,
    headers: &HeaderMap,
) -> std::result::Result<Actor, Box<Response>> {
    match state.host.viewer(headers).await {
        Ok(a) if a.member_id().is_some() => {
            if a.has(Permission::ModuleManage) {
                Ok(a)
            } else {
                Err(Box::new(
                    state
                        .host
                        .error(&module_error(
                            Code::Forbidden,
                            "예약 관리는 모듈 관리 권한이 있는 운영자만 봅니다.",
                        ))
                        .await,
                ))
            }
        }
        Ok(_) => Err(Box::new(
            Redirect::to(&urls::with_query("/login", &[("next", HOME)])).into_response(),
        )),
        Err(e) => Err(Box::new(state.host.error(&e).await)),
    }
}

async fn guard_post(
    state: &ModuleState,
    headers: &HeaderMap,
) -> std::result::Result<Actor, Box<Response>> {
    if !state.host.same_origin(headers) {
        return Err(Box::new(
            state
                .host
                .error(&module_error(
                    Code::Forbidden,
                    "이 사이트에서 보낸 폼이 아닙니다.",
                ))
                .await,
        ));
    }
    operator(state, headers).await
}

fn row_view(b: &BookingRow) -> Value {
    let mut v = b.detail();
    v["can_confirm"] = json!(b.status == REQUESTED);
    v["can_cancel"] = json!([HELD, REQUESTED, CONFIRMED].contains(&b.status.as_str()));
    v["refund_needed"] = json!(b.paid != 0 && b.refunded == 0 && b.status != CONFIRMED);
    v
}

impl BookingModule {
    async fn admin_view(&self, ctx: &Ctx, q: &AdminQuery) -> Result<Value> {
        self.settle_due(ctx).await?;
        let tz = settings::site(ctx).await?.tz_offset_min;
        let today = local_now(tz).0;
        let day = parse_day(&q.day).unwrap_or(today);
        let key = day_str(day);
        let shift = |n: i64| {
            day.checked_add(time::Duration::days(n))
                .map(day_str)
                .unwrap_or_else(|| key.clone())
        };
        let items: Vec<Value> = self
            .list(&key, &key, "")
            .await?
            .iter()
            .map(row_view)
            .collect();
        let waiting: Vec<Value> = self.waiting(tz).await?.iter().map(row_view).collect();
        let services: Vec<Value> = self
            .services()
            .await?
            .iter()
            .map(ServiceRow::view)
            .collect();
        let conf = self.conf(ctx).await?;
        Ok(json!({
            "noindex": true,
            "day": key,
            "day_text": day_label(day),
            "is_today": day == today,
            "today": day_str(today),
            "prev": shift(-1),
            "next": shift(1),
            "items": items,
            "waiting": waiting,
            "services": services,
            "hours": hours_view(&self.hours().await?),
            "blocks": self.blocks_from(&day_str(today)).await?,
            "conf": {
                "cancel_hours": conf.cancel_hours,
                "notice_minutes": conf.notice_minutes,
                "days_ahead": conf.days_ahead,
                "auto_confirm": conf.auto_confirm,
                "ask_phone": conf.ask_phone,
            },
            "done": done_text(&q.done),
        }))
    }
}

async fn render(
    state: &ModuleState,
    module: &BookingModule,
    q: &AdminQuery,
    extra: Value,
    status: StatusCode,
) -> Response {
    let mut v = match module.admin_view(&state.ctx, q).await {
        Ok(v) => v,
        Err(e) => return state.host.error(&e).await,
    };
    if let (Some(v), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in extra {
            v.insert(k.clone(), x.clone());
        }
    }
    let page = state
        .host
        .page("mod/booking/admin.html", "예약 관리", v)
        .await;
    no_store((status, page).into_response())
}

async fn fail(state: &ModuleState, module: &BookingModule, day: &str, e: &Error) -> Response {
    let status = match e.code() {
        Code::Forbidden => StatusCode::FORBIDDEN,
        Code::NotFound => StatusCode::NOT_FOUND,
        Code::Validation => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::CONFLICT,
    };
    let q = AdminQuery {
        day: day.to_owned(),
        done: String::new(),
    };
    render(state, module, &q, json!({ "error": e.message() }), status).await
}

fn back(day: &str, done: &str) -> Response {
    let mut pairs = vec![("done", done)];
    if !day.is_empty() {
        pairs.insert(0, ("day", day));
    }
    Redirect::to(&urls::with_query(HOME, &pairs)).into_response()
}

pub async fn page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Query(q): Query<AdminQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = operator(&state, &headers).await {
        return *r;
    }
    render(&state, &module, &q, json!({}), StatusCode::OK).await
}

#[derive(Debug, Default, Deserialize)]
pub struct DayForm {
    #[serde(default)]
    day: String,
    #[serde(default)]
    reason: String,
}

pub async fn confirm(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<DayForm>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    match module.confirm(&state.ctx, &actor, &id).await {
        Ok(_) => back(&f.day, "confirm"),
        Err(e) => fail(&state, &module, &f.day, &e).await,
    }
}

pub async fn cancel(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<DayForm>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    match module.cancel(&state.ctx, &actor, &id, &f.reason).await {
        Ok(v) if v["refund_needed"] == true => back(&f.day, "refund"),
        Ok(_) => back(&f.day, "cancel"),
        Err(e) => fail(&state, &module, &f.day, &e).await,
    }
}

fn number(v: Option<&String>) -> Option<i64> {
    let digits: String = v?.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub async fn service(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let input = ServiceSetInput {
        id: f.get("id").cloned().unwrap_or_default(),
        name: f.get("name").cloned(),
        description: f.get("description").cloned(),
        minutes: number(f.get("minutes")),
        capacity: number(f.get("capacity")),
        price: Some(number(f.get("price")).unwrap_or(0)),
        deposit: Some(number(f.get("deposit")).unwrap_or(0)),
        enabled: Some(f.contains_key("enabled")),
        position: number(f.get("position")),
    };
    match module.service_set(&actor, input).await {
        Ok(_) => back("", "service"),
        Err(e) => fail(&state, &module, "", &e).await,
    }
}

/// 일곱 요일을 한 번에. 칸 이름은 `open_mon`·`close_mon`·`closed_mon` 꼴.
pub async fn hours(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    for k in WEEKDAY_KEYS {
        let open = f.get(&format!("open_{k}")).cloned().unwrap_or_default();
        let close = f.get(&format!("close_{k}")).cloned().unwrap_or_default();
        let closed = f.contains_key(&format!("closed_{k}"))
            || (open.trim().is_empty() && close.trim().is_empty());
        let input = HoursInput {
            days: k.to_owned(),
            open: Some(open),
            close: Some(close),
            closed,
        };
        if let Err(e) = module.set_hours(&actor, input).await {
            return fail(&state, &module, "", &e).await;
        }
    }
    back("", "hours")
}

pub async fn block(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let input = BlockInput {
        day: f.get("day").cloned().unwrap_or_default(),
        start: f.get("start").cloned(),
        end: f.get("end").cloned(),
        reason: f.get("reason").cloned(),
    };
    match module.block(&state.ctx, &actor, input).await {
        Ok(_) => back("", "block"),
        Err(e) => fail(&state, &module, "", &e).await,
    }
}

pub async fn unblock(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    match module.unblock(&actor, &id).await {
        Ok(_) => back("", "unblock"),
        Err(e) => fail(&state, &module, "", &e).await,
    }
}
