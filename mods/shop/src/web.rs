//! 손님 화면 (ADR-0170): 상품 목록 `/m/shop`, 상품 `/p/<id>`, 장바구니 `/cart`, 주문서 `/checkout`,
//! 주문 화면 `/o/<id>`, 주문 조회 `/lookup`. 공개 화면의 폼은 쿠키 토큰이 없어 출처로 위조 요청을 막는다.
//! 장바구니 쿠키는 HttpOnly·SameSite=Lax·경로 `/m/shop`. 손님이 적은 값은 이스케이프해 다시 채운다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use maru_core::actor::Actor;
use maru_core::{Code, Ctx, Error, Result};
use maru_modules::{ModuleState, module_error};
use serde_json::{Value, json};

use super::{
    CART_COOKIE, CART_DAYS, HOME, Line, OrderRow, PENDING, PlaceInput, ProductRow, ShopModule,
    quote,
};

pub(crate) fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn status_of(e: &Error) -> StatusCode {
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

fn read_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == CART_COOKIE && !v.is_empty()).then(|| v.to_owned())
        })
}

fn cart_cookie(ctx: &Ctx, value: &str) -> HeaderValue {
    let mut c = format!(
        "{CART_COOKIE}={value}; HttpOnly; SameSite=Lax; Path={HOME}; Max-Age={}",
        CART_DAYS * 86_400
    );
    if ctx.site_url.starts_with("https://") {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
}

async fn page(
    state: &ModuleState,
    tpl: &str,
    title: &str,
    v: Value,
    status: StatusCode,
) -> Response {
    let res = state.host.page(tpl, title, v).await;
    no_store((status, res).into_response())
}

impl ShopModule {
    async fn cart_view(&self, ctx: &Ctx, cart: Option<&str>) -> Result<(Vec<Line>, Value)> {
        let conf = self.conf(ctx).await?;
        let lines = match cart {
            Some(id) => self.lines(id).await?,
            None => Vec::new(),
        };
        let q = quote(&lines, &conf);
        let v = json!({
            "noindex": true,
            "lines": lines.iter().map(Line::view).collect::<Vec<_>>(),
            "quote": q.view(&conf),
            "blocked": lines.iter().any(|l| l.problem().is_some()),
            "ask_phone": conf.ask_phone,
            "policy": conf.policy,
        });
        Ok((lines, v))
    }
}

pub async fn list(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
) -> Response {
    match module.products().await {
        Ok(all) => {
            let items: Vec<Value> = all
                .iter()
                .filter(|p| p.enabled != 0)
                .map(ProductRow::view)
                .collect();
            page(
                &state,
                "mod/shop/list.html",
                "상품",
                json!({ "products": items }),
                StatusCode::OK,
            )
            .await
        }
        Err(e) => state.host.error(&e).await,
    }
}

pub async fn product(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    Path(id): Path<String>,
) -> Response {
    match module.product(&id).await {
        Ok(p) if p.enabled != 0 => {
            let name = p.name.clone();
            page(
                &state,
                "mod/shop/product.html",
                &name,
                json!({ "p": p.view(), "description": p.description.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>() }),
                StatusCode::OK,
            )
            .await
        }
        Ok(_) => state.host.error(&super::no_product(&id)).await,
        Err(e) => state.host.error(&e).await,
    }
}

pub async fn cart(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
) -> Response {
    let key = read_cookie(&headers);
    let cart = match module.cart_id(key.as_deref()).await {
        Ok(c) => c,
        Err(e) => return state.host.error(&e).await,
    };
    match module.cart_view(&state.ctx, cart.as_deref()).await {
        Ok((_, v)) => page(&state, "mod/shop/cart.html", "장바구니", v, StatusCode::OK).await,
        Err(e) => state.host.error(&e).await,
    }
}

/// 담기(`do=add`)·수량 바꾸기(`do=set`). 장바구니가 없으면 만들고 쿠키를 준다.
pub async fn cart_post(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return foreign(&state).await;
    }
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    let qty: i64 = get("qty").trim().parse().unwrap_or(1);
    let key = read_cookie(&headers);
    let mut set_cookie = None;
    let cart = match module.cart_id(key.as_deref()).await {
        Ok(Some(id)) => id,
        Ok(None) if get("do") == "add" => {
            let member = state.host.viewer(&headers).await.ok();
            let member = member.as_ref().and_then(Actor::member_id).cloned();
            match module.cart_new(member.as_deref()).await {
                Ok((id, key)) => {
                    set_cookie = Some(cart_cookie(&state.ctx, &key));
                    id
                }
                Err(e) => return state.host.error(&e).await,
            }
        }
        Ok(None) => return Redirect::to(&format!("{HOME}/cart")).into_response(),
        Err(e) => return state.host.error(&e).await,
    };
    let done = if get("do") == "add" {
        module
            .cart_add(&cart, &get("product"), &get("option"), qty)
            .await
    } else {
        module
            .cart_set(&cart, &get("product"), &get("option"), qty)
            .await
    };
    let mut res = match done {
        Ok(()) => Redirect::to(&format!("{HOME}/cart")).into_response(),
        Err(e) if get("do") == "add" => {
            // 상품 화면에 까닭을 띄운다.
            let status = status_of(&e);
            match module.product(&get("product")).await {
                Ok(p) if p.enabled != 0 => {
                    page(
                        &state,
                        "mod/shop/product.html",
                        &p.name.clone(),
                        json!({ "p": p.view(), "description": p.description.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>(), "error": e.message(), "option": get("option") }),
                        status,
                    )
                    .await
                }
                _ => state.host.error(&e).await,
            }
        }
        Err(e) => state.host.error(&e).await,
    };
    if let Some(c) = set_cookie {
        res.headers_mut().append(header::SET_COOKIE, c);
    }
    res
}

async fn checkout_view(
    state: &ModuleState,
    module: &ShopModule,
    headers: &HeaderMap,
    sent: &BTreeMap<String, String>,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let key = read_cookie(headers);
    let cart = match module.cart_id(key.as_deref()).await {
        Ok(c) => c,
        Err(e) => return state.host.error(&e).await,
    };
    let (lines, mut v) = match module.cart_view(&state.ctx, cart.as_deref()).await {
        Ok(x) => x,
        Err(e) => return state.host.error(&e).await,
    };
    if lines.is_empty() && error.is_none() {
        return Redirect::to(&format!("{HOME}/cart")).into_response();
    }
    let val = |k: &str| sent.get(k).cloned().unwrap_or_default();
    v["error"] = json!(error);
    v["values"] = json!({
        "name": val("name"), "email": val("email"), "phone": val("phone"), "zip": val("zip"),
        "address": val("address"), "address2": val("address2"), "memo": val("memo"),
        "agree": val("agree") == "1",
    });
    page(state, "mod/shop/checkout.html", "주문서", v, status).await
}

pub async fn checkout(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
) -> Response {
    checkout_view(
        &state,
        &module,
        &headers,
        &BTreeMap::new(),
        None,
        StatusCode::OK,
    )
    .await
}

pub async fn place(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
    Form(sent): Form<BTreeMap<String, String>>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return foreign(&state).await;
    }
    // 사람에게 보이지 않는 칸을 채운 자동 프로그램은 주문을 만들지 않고 처음 화면으로.
    if sent.get("website").is_some_and(|v| !v.is_empty()) {
        return Redirect::to("/").into_response();
    }
    let get = |k: &str| sent.get(k).cloned().unwrap_or_default();
    let key = read_cookie(&headers);
    let result = if get("agree") != "1" {
        Err(module_error(
            Code::Validation,
            "주문 내용과 개인정보 수집에 동의해야 주문할 수 있습니다.",
        ))
    } else {
        match module.cart_id(key.as_deref()).await {
            Ok(Some(cart)) => {
                let viewer = state
                    .host
                    .viewer(&headers)
                    .await
                    .unwrap_or_else(|_| Actor::anonymous([]));
                let input = PlaceInput {
                    name: get("name"),
                    email: get("email"),
                    phone: get("phone"),
                    zip: get("zip"),
                    address: get("address"),
                    address2: get("address2"),
                    memo: get("memo"),
                    member_id: viewer.member_id().cloned(),
                };
                module.place(&state.ctx, &cart, input).await
            }
            Ok(None) => Err(module_error(Code::Validation, "장바구니가 비었습니다.")),
            Err(e) => Err(e),
        }
    };
    match result {
        Ok((_, next)) => Redirect::to(&next).into_response(),
        Err(e) => {
            let status = status_of(&e);
            checkout_view(&state, &module, &headers, &sent, Some(e.message()), status).await
        }
    }
}

fn order_page(o: &OrderRow, items: Value) -> Value {
    json!({ "noindex": true, "o": items, "pending": o.status == PENDING })
}

/// 결제 뒤 돌아오는 주문 화면. 주문 번호(무작위 24자)만 알면 보이므로 구매자 정보는 싣지 않는다.
pub async fn done(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    Path(id): Path<String>,
) -> Response {
    let o = match module.fresh(&state.ctx, &id).await {
        Ok(o) => o,
        Err(e) => return state.host.error(&e).await,
    };
    let items = match module.items(&o.id).await {
        Ok(i) => i,
        Err(e) => return state.host.error(&e).await,
    };
    let v = order_page(&o, o.public(&items));
    page(&state, "mod/shop/order.html", "주문", v, StatusCode::OK).await
}

async fn lookup_view(
    state: &ModuleState,
    module: &ShopModule,
    headers: &HeaderMap,
    mut v: Value,
    status: StatusCode,
) -> Response {
    v["noindex"] = json!(true);
    // 로그인한 회원은 자기 주문을 바로 본다.
    if let Ok(viewer) = state.host.viewer(headers).await
        && let Some(member) = viewer.member_id()
    {
        match module.member_orders(member).await {
            Ok(rows) => {
                let mine: Vec<Value> = rows
                    .iter()
                    .map(|o| {
                        json!({
                            "id": o.id, "status_label": super::label(&o.status),
                            "total_text": super::won(o.total), "created_at": o.created_at,
                            "url": format!("{HOME}/o/{}", o.id),
                        })
                    })
                    .collect();
                v["mine"] = json!(mine);
            }
            Err(e) => return state.host.error(&e).await,
        }
    }
    page(state, "mod/shop/lookup.html", "주문 조회", v, status).await
}

pub async fn lookup_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
) -> Response {
    lookup_view(&state, &module, &headers, json!({}), StatusCode::OK).await
}

pub async fn lookup(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ShopModule>>,
    headers: HeaderMap,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return foreign(&state).await;
    }
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    match module.lookup(&state.ctx, &get("id"), &get("email")).await {
        Ok(o) => match module.items(&o.id).await {
            Ok(items) => {
                let v = json!({ "found": o.detail(&items), "id": o.id });
                lookup_view(&state, &module, &headers, v, StatusCode::OK).await
            }
            Err(e) => state.host.error(&e).await,
        },
        Err(_) => {
            let v = json!({
                "error": "주문을 찾지 못했습니다. 주문 번호와 주문할 때 적은 전자우편 주소를 확인하세요.",
                "id": get("id"), "email": get("email"),
            });
            lookup_view(&state, &module, &headers, v, StatusCode::NOT_FOUND).await
        }
    }
}
