//! 결제 관리 화면 `/m/pay` (ADR-0167). 주문 표·입금 확인·환불·결제 요청 링크·결제 방법 설정.
//!
//! 툴과 같은 함수를 부른다(원칙 1). 보기·설정은 모듈 관리 권한, 입금 확인·환불은 그 안에서 사람만 받는다.
//! 구매자 정보는 바깥 사람이 쓴 값이라 이스케이프해 그린다.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use maru_core::actor::{Actor, Permission};
use maru_core::services::settings;
use maru_core::{Code, Error, Result, urls};
use maru_modules::checkout::CheckoutRequest;
use maru_modules::{ModuleState, module_error};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    AWAITING, EXPIRED, HOME, MANIFEST, PAID, PARTIAL, PENDING, PER_PAGE, PayModule,
    ProviderSetInput, REFUNDED, label, local_time, no_store, provider_view, won,
};

const STATUSES: [&str; 6] = [PENDING, AWAITING, PAID, PARTIAL, REFUNDED, EXPIRED];

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    status: String,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    done: String,
}

fn done_text(done: &str) -> Option<&'static str> {
    Some(match done {
        "paid" => "입금 확인으로 결제 완료가 됐습니다.",
        "refund" => "환불했습니다.",
        "sync" => "결제사에 다시 물어 맞췄습니다.",
        "provider" => "결제 방법을 저장했습니다.",
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
                            "결제 관리는 모듈 관리 권한이 있는 운영자만 봅니다.",
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

impl PayModule {
    async fn admin_view(&self, ctx: &maru_core::Ctx, q: &ListQuery) -> Result<Value> {
        self.expire_due().await?;
        let status = if STATUSES.contains(&q.status.as_str()) {
            q.status.as_str()
        } else {
            ""
        };
        let page = q.page.unwrap_or(1).max(1);
        let (rows, total) = self.list(status, page).await?;
        let tz = settings::site(ctx).await.map_or(540, |s| s.tz_offset_min);
        let mut items = Vec::new();
        for o in &rows {
            let refunds: Vec<Value> = self
                .refunds(&o.id)
                .await?
                .into_iter()
                .map(|mut r| {
                    r["amount_text"] = json!(won(r["amount"].as_i64().unwrap_or(0)));
                    r["at"] = json!(local_time(tz, r["created_at"].as_str().unwrap_or("")));
                    r
                })
                .collect();
            items.push(json!({
                "id": o.id,
                "name": o.name,
                "owner": o.owner,
                "amount_text": won(o.amount),
                "left": o.amount - o.refunded,
                "left_text": won(o.amount - o.refunded),
                "refunded_text": won(o.refunded),
                "refunded": o.refunded,
                "status": o.status,
                "status_label": label(&o.status),
                "provider": o.provider,
                "method": o.method,
                "buyer_name": o.buyer_name,
                "buyer_email": o.buyer_email,
                "buyer_phone": o.buyer_phone,
                "depositor": o.depositor,
                "created": local_time(tz, &o.created_at),
                "paid": o.paid_at.as_deref().map(|p| local_time(tz, p)),
                "expires": local_time(tz, &o.expires_at),
                "last_error": o.last_error,
                "can_mark_paid": o.status == AWAITING && o.provider == "bank",
                "can_refund": o.status == PAID || o.status == PARTIAL,
                "can_sync": o.provider == "toss" && o.payment_key.is_some(),
                "refunds": refunds,
            }));
        }
        let providers: Vec<Value> = self
            .providers()
            .await?
            .iter()
            .map(|p| json!(provider_view(ctx, p)))
            .collect();
        let tabs: Vec<Value> = STATUSES
            .iter()
            .map(|s| json!({ "id": s, "label": label(s) }))
            .collect();
        let business = settings::site(ctx)
            .await
            .map(|s| s.business)
            .unwrap_or_default();
        let base = if status.is_empty() {
            HOME.to_owned()
        } else {
            urls::with_query(HOME, &[("status", status)])
        };
        Ok(json!({
            "noindex": true,
            "items": items,
            "total": total,
            "status": status,
            "tabs": tabs,
            "providers": providers,
            "missing": business.missing(),
            "base": base,
            "prev": (page > 1).then_some(page - 1),
            "next": (page * PER_PAGE < total).then_some(page + 1),
            "done": done_text(&q.done),
        }))
    }
}

async fn render(
    state: &ModuleState,
    module: &PayModule,
    q: &ListQuery,
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
    let page = state.host.page("mod/pay/admin.html", "결제 관리", v).await;
    no_store((status, page).into_response())
}

async fn fail(state: &ModuleState, module: &PayModule, e: &Error) -> Response {
    let status = match e.code() {
        Code::Forbidden => StatusCode::FORBIDDEN,
        Code::NotFound => StatusCode::NOT_FOUND,
        Code::Validation => StatusCode::UNPROCESSABLE_ENTITY,
        Code::External => StatusCode::BAD_GATEWAY,
        _ => StatusCode::CONFLICT,
    };
    render(
        state,
        module,
        &ListQuery::default(),
        json!({ "error": e.message() }),
        status,
    )
    .await
}

pub async fn page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = operator(&state, &headers).await {
        return *r;
    }
    render(&state, &module, &q, json!({}), StatusCode::OK).await
}

fn back(done: &str) -> Response {
    Redirect::to(&urls::with_query(HOME, &[("done", done)])).into_response()
}

pub async fn mark_paid(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    match module.mark_paid(&actor, &id).await {
        Ok(_) => back("paid"),
        Err(e) => fail(&state, &module, &e).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct RefundForm {
    #[serde(default)]
    amount: String,
    #[serde(default)]
    reason: String,
}

pub async fn refund(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<RefundForm>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let digits: String = f.amount.chars().filter(char::is_ascii_digit).collect();
    let amount = if digits.is_empty() {
        None
    } else {
        digits.parse::<i64>().ok()
    };
    match module.refund(&actor, &id, amount, &f.reason).await {
        Ok(_) => back("refund"),
        Err(e) => fail(&state, &module, &e).await,
    }
}

pub async fn sync(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = guard_post(&state, &headers).await {
        return *r;
    }
    let done = async {
        let o = module.fresh(&id).await?;
        let key = o.payment_key.clone().ok_or_else(|| {
            module_error(Code::Conflict, "아직 결제사로 결제하지 않은 주문입니다.")
        })?;
        module.sync(&o, &key).await
    }
    .await;
    match done {
        Ok(_) => back("sync"),
        Err(e) => fail(&state, &module, &e).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct ProviderForm {
    #[serde(default)]
    enabled: Option<String>,
    #[serde(default)]
    client_key: String,
    /// 비우면 저장된 키를 그대로 둔다
    #[serde(default)]
    secret_key: String,
    #[serde(default)]
    bank: String,
    #[serde(default)]
    account: String,
    #[serde(default)]
    holder: String,
}

pub async fn provider(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<ProviderForm>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let toss = id == "toss";
    let input = ProviderSetInput {
        provider: id,
        enabled: Some(f.enabled.is_some()),
        client_key: toss.then(|| f.client_key.clone()),
        secret_key: (toss && !f.secret_key.trim().is_empty()).then(|| f.secret_key.clone()),
        bank: (!toss).then(|| f.bank.clone()),
        account: (!toss).then(|| f.account.clone()),
        holder: (!toss).then(|| f.holder.clone()),
    };
    match module.provider_set(&state.ctx, &actor, input).await {
        Ok(_) => back("provider"),
        Err(e) => fail(&state, &module, &e).await,
    }
}

#[derive(Debug, Deserialize)]
pub struct NewForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    buyer_name: String,
    #[serde(default)]
    buyer_email: String,
    #[serde(default)]
    buyer_phone: String,
}

/// 결제 요청 링크를 만들어 한 번만 보여 준다(1회용 값은 다시 만들 수 없다).
pub async fn new_link(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    headers: HeaderMap,
    Form(f): Form<NewForm>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let digits: String = f.amount.chars().filter(char::is_ascii_digit).collect();
    let req = CheckoutRequest {
        owner: MANIFEST.id.into(),
        name: f.name,
        amount: digits.parse::<i64>().unwrap_or(0),
        buyer_name: f.buyer_name,
        buyer_email: f.buyer_email,
        buyer_phone: f.buyer_phone,
        ..CheckoutRequest::default()
    };
    match module.create(&state.ctx, &actor, req).await {
        Ok(started) => {
            render(
                &state,
                &module,
                &ListQuery::default(),
                json!({ "created": { "url": started.pay_url, "name": started.order.name, "amount_text": won(started.order.amount) } }),
                StatusCode::OK,
            )
            .await
        }
        Err(e) => fail(&state, &module, &e).await,
    }
}
