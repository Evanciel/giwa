//! 손님 화면 (ADR-0169): 날짜·시간 고르기와 연락처 `/m/booking/new`, 접수 화면 `/done/<id>`,
//! 1회용 취소 링크 `/c/<id>?k=`. 공개 화면의 폼은 쿠키 토큰이 없어 출처로 위조 요청을 막는다.
//! 손님이 적은 값은 바깥 값이라 이스케이프해 다시 채운다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use giwa_core::actor::Actor;
use giwa_core::services::settings;
use giwa_core::{Code, Ctx, Result, urls};
use giwa_modules::{ModuleState, module_error};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    BookInput, BookingModule, BookingRow, CANCELED, HELD, HOLD_MINUTES, HOME, ServiceRow,
    day_label, day_str, guest_can_cancel, local_now, parse_day,
};

pub(crate) fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn status_of(e: &giwa_core::Error) -> StatusCode {
    match e.code() {
        Code::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        Code::Validation => StatusCode::UNPROCESSABLE_ENTITY,
        Code::NotFound => StatusCode::NOT_FOUND,
        Code::Forbidden => StatusCode::FORBIDDEN,
        _ => StatusCode::CONFLICT,
    }
}

async fn foreign(state: &ModuleState) -> Response {
    state
        .host
        .error(&module_error(
            Code::Forbidden,
            "이 사이트에서 보낸 폼이 아닙니다.",
        ))
        .await
}

#[derive(Debug, Default, Deserialize)]
pub struct NewQuery {
    #[serde(default)]
    service: String,
    #[serde(default)]
    day: String,
}

impl BookingModule {
    /// 예약 화면 값. 서비스·날짜를 못 고른 단계면 고르는 목록만 낸다.
    async fn new_view(
        &self,
        ctx: &Ctx,
        service: &str,
        day: &str,
        sent: &BTreeMap<String, String>,
        error: Option<String>,
    ) -> Result<Value> {
        let conf = self.conf(ctx).await?;
        let tz = settings::site(ctx).await?.tz_offset_min;
        let services: Vec<ServiceRow> = self
            .services()
            .await?
            .into_iter()
            .filter(|s| s.enabled)
            .collect();
        if services.is_empty() {
            return Err(module_error(Code::NotFound, "지금은 예약을 받지 않습니다."));
        }
        let svc = services
            .iter()
            .find(|s| s.id == service.trim())
            .or_else(|| (services.len() == 1).then(|| &services[0]));
        let open = self.open_days(&conf, tz).await?;
        let chosen = parse_day(day).ok().or_else(|| open.first().copied());
        let link = |sid: &str, d: &str| {
            urls::with_query(&format!("{HOME}/new"), &[("service", sid), ("day", d)])
        };
        let mut out = json!({
            "noindex": true,
            "services": services.iter().map(|s| {
                let mut v = s.view();
                v["url"] = json!(urls::with_query(&format!("{HOME}/new"), &[("service", &s.id)]));
                v["current"] = json!(svc.is_some_and(|c| c.id == s.id));
                v
            }).collect::<Vec<_>>(),
            "many": services.len() > 1,
            "ask_phone": conf.ask_phone,
            "policy": conf.policy,
            "cancel_hours": conf.cancel_hours,
            "hold_minutes": HOLD_MINUTES,
            "error": error,
            "values": {
                "name": sent.get("name").cloned().unwrap_or_default(),
                "email": sent.get("email").cloned().unwrap_or_default(),
                "phone": sent.get("phone").cloned().unwrap_or_default(),
                "memo": sent.get("memo").cloned().unwrap_or_default(),
                "start": sent.get("start").cloned().unwrap_or_default(),
                "agree": sent.get("agree").is_some_and(|v| v == "1"),
            },
            "today": day_str(local_now(tz).0),
        });
        let Some(svc) = svc else {
            return Ok(out);
        };
        out["svc"] = svc.view();
        let days: Vec<Value> = open
            .iter()
            .map(|d| {
                let key = day_str(*d);
                json!({
                    "day": key,
                    "label": day_label(*d),
                    "url": link(&svc.id, &key),
                    "current": Some(*d) == chosen,
                })
            })
            .collect();
        out["days"] = json!(days);
        if let Some(d) = chosen {
            let (slots, why) = self.day_slots(svc, d, &conf, tz).await?;
            out["day"] = json!(day_str(d));
            out["day_text"] = json!(day_label(d));
            out["slots"] = json!(
                slots
                    .iter()
                    .map(|s| json!({ "start": s.start, "end": s.end, "left": s.left, "full": s.left <= 0 }))
                    .collect::<Vec<_>>()
            );
            out["reason"] = json!(why);
            out["any_left"] = json!(slots.iter().any(|s| s.left > 0));
        }
        Ok(out)
    }
}

pub async fn new_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Query(q): Query<NewQuery>,
) -> Response {
    match module
        .new_view(&state.ctx, &q.service, &q.day, &BTreeMap::new(), None)
        .await
    {
        Ok(v) => no_store(state.host.page("mod/booking/new.html", "예약", v).await),
        Err(e) => state.host.error(&e).await,
    }
}

pub async fn submit(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    headers: HeaderMap,
    Form(sent): Form<BTreeMap<String, String>>,
) -> Response {
    let ctx = &state.ctx;
    if !state.host.same_origin(&headers) {
        return foreign(&state).await;
    }
    // 사람에게 보이지 않는 칸을 채운 자동 프로그램은 예약을 만들지 않고 처음 화면으로.
    if sent.get("website").is_some_and(|v| !v.is_empty()) {
        return Redirect::to("/").into_response();
    }
    let get = |k: &str| sent.get(k).cloned().unwrap_or_default();
    let result = if get("agree") != "1" {
        Err(module_error(
            Code::Validation,
            "예약 내용과 개인정보 수집에 동의해야 예약할 수 있습니다.",
        ))
    } else {
        let viewer = state
            .host
            .viewer(&headers)
            .await
            .unwrap_or_else(|_| Actor::anonymous([]));
        let input = BookInput {
            service: get("service"),
            day: get("day"),
            start: get("start"),
            name: get("name"),
            email: get("email"),
            phone: get("phone"),
            memo: get("memo"),
            member_id: viewer.member_id().cloned(),
        };
        module.book(ctx, input).await
    };
    match result {
        Ok(b) => Redirect::to(&b.next).into_response(),
        Err(e) => {
            let status = status_of(&e);
            match module
                .new_view(ctx, &get("service"), &get("day"), &sent, Some(e.message()))
                .await
            {
                Ok(v) => {
                    let page = state.host.page("mod/booking/new.html", "예약", v).await;
                    no_store((status, page).into_response())
                }
                Err(e) => state.host.error(&e).await,
            }
        }
    }
}

/// 접수 화면. 예약 ID(무작위 24자)만 알면 보이므로 손님 정보는 싣지 않는다.
pub async fn done(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Path(id): Path<String>,
) -> Response {
    let b = match module.fresh(&state.ctx, &id).await {
        Ok(b) => b,
        Err(e) => return state.host.error(&e).await,
    };
    let v = json!({ "noindex": true, "b": b.public(), "held": b.status == HELD });
    no_store(state.host.page("mod/booking/done.html", "예약", v).await)
}

#[derive(Debug, Default, Deserialize)]
pub struct KeyQuery {
    #[serde(default)]
    k: String,
}

async fn cancel_view(module: &BookingModule, ctx: &Ctx, b: &BookingRow, k: &str) -> Value {
    let hours = module.conf(ctx).await.map_or(24, |c| c.cancel_hours);
    json!({
        "noindex": true,
        "b": b.public(),
        "k": k,
        "can_cancel": guest_can_cancel(b, hours),
        "cancel_hours": hours,
    })
}

pub async fn cancel_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Path(id): Path<String>,
    Query(q): Query<KeyQuery>,
) -> Response {
    let b = match module.guest_view(&state.ctx, &id, &q.k).await {
        Ok(b) => b,
        Err(e) => return state.host.error(&e).await,
    };
    let v = cancel_view(&module, &state.ctx, &b, &q.k).await;
    no_store(
        state
            .host
            .page("mod/booking/cancel.html", "예약 취소", v)
            .await,
    )
}

pub async fn cancel_submit(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<BookingModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<KeyQuery>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return foreign(&state).await;
    }
    match module.guest_cancel(&state.ctx, &id, &f.k).await {
        Ok(b) => {
            let mut v = cancel_view(&module, &state.ctx, &b, "").await;
            v["done"] = json!(b.status == CANCELED);
            v["refund"] = json!(b.paid != 0);
            no_store(
                state
                    .host
                    .page("mod/booking/cancel.html", "예약 취소", v)
                    .await,
            )
        }
        Err(e) if e.code() == Code::Conflict => {
            let Ok(b) = module.guest_view(&state.ctx, &id, &f.k).await else {
                return state.host.error(&e).await;
            };
            let mut v = cancel_view(&module, &state.ctx, &b, &f.k).await;
            v["error"] = json!(e.message());
            let page = state
                .host
                .page("mod/booking/cancel.html", "예약 취소", v)
                .await;
            no_store((StatusCode::CONFLICT, page).into_response())
        }
        Err(e) => state.host.error(&e).await,
    }
}
