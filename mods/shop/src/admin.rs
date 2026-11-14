//! 쇼핑 관리 화면 `/m/shop/admin` (ADR-0170). 주문(상태 탭·배송 상태·취소)과 상품.
//!
//! 툴과 같은 함수를 부른다(원칙 1). 보기·상품·배송 상태는 모듈 관리 권한, 취소는 그 안에서 사람만 받는다.
//! 구매자 정보는 바깥 사람이 쓴 값이라 이스케이프해 그린다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use giwa_core::actor::{Actor, Permission};
use giwa_core::{Code, Ctx, Error, Result, urls};
use giwa_modules::{ModuleState, module_error};
use serde::Deserialize;
use serde_json::{Value, json};

use super::web::no_store;
use super::{
    ADMIN, CANCELED, DELIVERED, EXPIRED, OrderRow, PAID, PENDING, PREPARING, ProductRow,
    ProductSetInput, SHIPPED, ShopModule, StatusInput,
};

const TABS: [(&str, &str); 6] = [
    ("to_ship", "보낼 주문"),
    ("shipped", "배송 중"),
    ("delivered", "배송 완료"),
    ("pending", "결제 대기"),
    ("canceled", "취소"),
    ("", "전체"),
];

#[derive(Debug, Default, Deserialize)]
pub struct AdminQuery {
    status: Option<String>,
    #[serde(default)]
    done: String,
}

fn done_text(done: &str) -> Option<&'static str> {
    Some(match done {
        "status" => "배송 상태를 바꾸고 구매자에게 알렸습니다.",
        "cancel" => "주문을 취소했습니다.",
        "refund" => "주문을 취소했습니다. 결제 금액은 결제 관리에서 환불하세요.",
        "product" => "상품을 저장했습니다.",
        _ => return None,
    })
}

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
                            "쇼핑 관리는 모듈 관리 권한이 있는 운영자만 봅니다.",
                        ))
                        .await,
                ))
            }
        }
        Ok(_) => Err(Box::new(
            Redirect::to(&urls::with_query("/login", &[("next", ADMIN)])).into_response(),
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

fn row_view(o: &OrderRow, mut v: Value) -> Value {
    let short = o.short != 0;
    v["can_prepare"] = json!(o.status == PAID && !short);
    v["can_ship"] = json!([PAID, PREPARING].contains(&o.status.as_str()) && !short);
    v["can_deliver"] = json!(o.status == SHIPPED);
    v["can_cancel"] = json!([PENDING, PAID, PREPARING].contains(&o.status.as_str()));
    v["refund_needed"] = json!(
        o.paid_at.is_some()
            && o.refunded == 0
            && ([CANCELED, EXPIRED].contains(&o.status.as_str()) || short)
    );
    v
}

impl ShopModule {
    async fn admin_view(&self, ctx: &Ctx, q: &AdminQuery) -> Result<Value> {
        self.settle_due(ctx).await?;
        let status = q.status.clone().unwrap_or_else(|| "to_ship".into());
        let rows = self.list(&status, 100).await?;
        let mut orders = Vec::with_capacity(rows.len());
        for o in &rows {
            let items = self.items(&o.id).await?;
            orders.push(row_view(o, o.detail(&items)));
        }
        let tabs: Vec<Value> = TABS
            .iter()
            .map(|(k, l)| {
                json!({
                    "label": l,
                    "url": urls::with_query(ADMIN, &[("status", k)]),
                    "current": *k == status,
                })
            })
            .collect();
        let products: Vec<Value> = self
            .products()
            .await?
            .iter()
            .map(ProductRow::view)
            .collect();
        let conf = self.conf(ctx).await?;
        Ok(json!({
            "noindex": true,
            "status": status,
            "tabs": tabs,
            "orders": orders,
            "products": products,
            "conf": { "shipping_fee": super::won(conf.shipping_fee), "free_over": (conf.free_over > 0).then(|| super::won(conf.free_over)) },
            "done": done_text(&q.done),
            "delivered": DELIVERED,
        }))
    }
}

async fn render(
    state: &ModuleState,
    module: &ShopModule,
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
    let page = state.host.page("mod/shop/admin.html", "쇼핑 관리", v).await;
    no_store((status, page).into_response())
}

async fn fail(state: &ModuleState, module: &ShopModule, tab: &str, e: &Error) -> Response {
    let status = match e.code() {
        Code::Forbidden => StatusCode::FORBIDDEN,
        Code::NotFound => StatusCode::NOT_FOUND,
        Code::Validation => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::CONFLICT,
    };
    let q = AdminQuery {
        status: (!tab.is_empty()).then(|| tab.to_owned()),
        done: String::new(),
    };
    render(state, module, &q, json!({ "error": e.message() }), status).await
}

fn back(tab: &str, done: &str) -> Response {
    let mut pairs = vec![("done", done)];
    if !tab.is_empty() {
        pairs.insert(0, ("status", tab));
    }
    Redirect::to(&urls::with_query(ADMIN, &pairs)).into_response()
}

pub async fn page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    Query(q): Query<AdminQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = operator(&state, &headers).await {
        return *r;
    }
    render(&state, &module, &q, json!({}), StatusCode::OK).await
}

pub async fn status(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    let input = StatusInput {
        id,
        status: get("to"),
        carrier: get("carrier"),
        tracking: get("tracking"),
    };
    match module.set_status(&state.ctx, &actor, input).await {
        Ok(_) => back(&get("tab"), "status"),
        Err(e) => fail(&state, &module, &get("tab"), &e).await,
    }
}

pub async fn cancel(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    match module.cancel(&state.ctx, &actor, &id, &get("reason")).await {
        Ok(v) if v["refund_needed"] == true => back(&get("tab"), "refund"),
        Ok(_) => back(&get("tab"), "cancel"),
        Err(e) => fail(&state, &module, &get("tab"), &e).await,
    }
}

fn number(v: Option<&String>) -> Option<i64> {
    let v = v?.trim();
    if v == "-1" {
        return Some(-1);
    }
    let digits: String = v.chars().filter(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// 상품 저장. 재고 칸을 비우면 세지 않는다(무제한).
pub async fn product(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let actor = match guard_post(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let input = ProductSetInput {
        id: f.get("id").cloned().unwrap_or_default(),
        name: f.get("name").cloned(),
        description: f.get("description").cloned(),
        price: number(f.get("price")),
        stock: Some(number(f.get("stock")).unwrap_or(-1)),
        image: f.get("image").cloned(),
        option_label: f.get("option_label").cloned(),
        options: f.get("options").cloned(),
        enabled: Some(f.contains_key("enabled")),
        position: number(f.get("position")),
    };
    match module.product_set(&actor, input).await {
        Ok(_) => back("", "product"),
        Err(e) => fail(&state, &module, "", &e).await,
    }
}
