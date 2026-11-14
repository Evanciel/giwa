//! 결제 단추 위젯 (ADR-0167). 상담비·수강료·후원처럼 상품 목록 없이 받는 결제.
//!
//! 금액은 위젯 설정이 정한다. 「금액 직접」 단추만 구매자가 적고, 그때도 운영자가 정한 최소·최대 안에서만 받는다.
//! 단추를 누르면 주문을 만들고 주문 화면(1회용 주소)으로 보낸다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use giwa_core::actor::Actor;
use giwa_core::services::auth;
use giwa_core::{Code, Ctx, Error, Result, clock};
use giwa_modules::checkout::CheckoutRequest;
use giwa_modules::{ModuleState, module_error};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{MANIFEST, MAX_AMOUNT, PayModule, bad, email, local_path, phone, text, won};

pub const KIND: &str = "pay_button";
/// 단추로 만든 주문은 한 시간 안에 결제한다 (계획서: pending 은 1시간 뒤 expired).
const BUTTON_EXPIRES_MINUTES: i64 = 60;
/// 1분에 이만큼 넘게 단추 주문이 오면 잠시 막는다.
const PER_MINUTE: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ButtonWidget {
    /// 이 단추의 이름(영문 소문자·숫자·-, 40자까지). 비우면 서버가 만든다
    #[serde(default)]
    pub key: String,
    /// 주문 이름. 결제 창과 확인 편지에 보인다 (예: 첫 상담 30분)
    pub name: String,
    /// 원 단위 금액. 0 이면 구매자가 최소~최대 사이에서 적는다(후원 등)
    #[serde(default)]
    pub amount: i64,
    /// 금액을 구매자가 적을 때 최소 (원)
    #[serde(default)]
    pub min: i64,
    /// 금액을 구매자가 적을 때 최대 (원)
    #[serde(default)]
    pub max: i64,
    /// 단추 글자. 비우면 「결제하기」
    #[serde(default)]
    pub label: String,
    /// 단추 위 설명 한두 문장
    #[serde(default)]
    pub note: String,
    /// 전화번호도 받는다
    #[serde(default)]
    pub ask_phone: bool,
    /// 환불 규정 페이지 주소(사이트 안). 있으면 동의 칸에 연결한다
    #[serde(default)]
    pub policy: String,
    /// 결제를 마친 뒤 돌아갈 사이트 안 주소. 비우면 주문 화면
    #[serde(default)]
    pub return_to: String,
    /// 구역 이름(영문 소문자·숫자·-). 주면 `id="s-<anchor>"` 가 붙어 `/#s-<anchor>` 링크로 내려온다
    #[serde(default)]
    pub anchor: String,
    /// 띠 색: plain, soft, strong
    #[serde(default = "plain")]
    pub tone: String,
}

fn plain() -> String {
    "plain".into()
}

fn key_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 40
        && k.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// 저장 전에 고르게 만든다. 빈 이름은 무작위로 채운다.
pub fn check(mut c: ButtonWidget) -> Result<ButtonWidget> {
    c.key = c.key.trim().to_owned();
    if c.key.is_empty() {
        c.key = auth::new_secret()?[..10].to_owned();
    }
    if !key_ok(&c.key) {
        return Err(bad(
            "단추 이름(key)은 영문 소문자·숫자·- 로 40자까지입니다.",
        ));
    }
    c.name = text("주문 이름", &c.name, 1, 100)?;
    c.label = text("단추 글자", &c.label, 0, 30)?;
    c.note = text("설명", &c.note, 0, 300)?;
    c.policy = local_path(&c.policy)?;
    c.return_to = local_path(&c.return_to)?;
    c.anchor = c.anchor.trim().to_ascii_lowercase();
    if !c.anchor.is_empty() && !key_ok(&c.anchor) {
        return Err(bad(
            "구역 이름(anchor)은 영문 소문자·숫자·- 로 40자까지입니다.",
        ));
    }
    if !["plain", "soft", "strong"].contains(&c.tone.as_str()) {
        return Err(bad("tone 은 plain·soft·strong 입니다."));
    }
    if c.amount == 0 {
        if c.min < 1 {
            c.min = 1000;
        }
        if c.max == 0 {
            c.max = 1_000_000;
        }
        if c.min > c.max || c.max > MAX_AMOUNT {
            return Err(bad(format!(
                "금액을 구매자가 적을 때는 최소 ≤ 최대 ≤ {MAX_AMOUNT}원입니다."
            )));
        }
    } else {
        if !(1..=MAX_AMOUNT).contains(&c.amount) {
            return Err(bad(format!("금액은 1~{MAX_AMOUNT}원입니다.")));
        }
        c.min = 0;
        c.max = 0;
    }
    Ok(c)
}

/// 위젯·다시 쓰기 화면이 그릴 값.
pub fn view(c: &ButtonWidget, sent: &BTreeMap<String, String>, error: Option<String>) -> Value {
    let v = |k: &str| sent.get(k).cloned().unwrap_or_default();
    json!({
        "section": true,
        "tone": c.tone,
        "anchor": (!c.anchor.is_empty()).then(|| c.anchor.clone()),
        "key": c.key,
        "name": c.name,
        "note": c.note,
        "amount": c.amount,
        "amount_text": won(c.amount),
        "custom": c.amount == 0,
        "min": c.min,
        "max": c.max,
        "min_text": won(c.min),
        "max_text": won(c.max),
        "label": if c.label.is_empty() { "결제하기".to_owned() } else { c.label.clone() },
        "ask_phone": c.ask_phone,
        "policy": c.policy,
        "action": format!("{}/button/{}", super::HOME, c.key),
        "values": {
            "buyer_name": v("buyer_name"),
            "buyer_email": v("buyer_email"),
            "buyer_phone": v("buyer_phone"),
            "amount": v("amount"),
            "agree": v("agree") == "1",
        },
        "error": error,
    })
}

impl PayModule {
    /// 켜진 결제 단추 중 이름이 같은 것.
    async fn button(&self, ctx: &Ctx, key: &str) -> Result<ButtonWidget> {
        if key_ok(key) {
            for w in ctx.store.list_widgets().await? {
                if w.kind == KIND && w.enabled {
                    let c: ButtonWidget =
                        serde_json::from_value(w.config.clone()).map_err(Error::internal)?;
                    if c.key == key {
                        return Ok(c);
                    }
                }
            }
        }
        Err(module_error(Code::NotFound, "없는 결제 단추입니다."))
    }

    async fn recent_button_orders(&self) -> Result<i64> {
        let since = clock::minutes_ago(1);
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!: i64" FROM mod_pay_order
               WHERE ref_id LIKE 'button:%' AND created_at > ?"#,
            since
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)
    }

    /// 단추 폼으로 주문을 만들고 주문 화면 주소(사이트 안)를 돌려준다.
    async fn button_order(
        &self,
        ctx: &Ctx,
        c: &ButtonWidget,
        viewer: &Actor,
        sent: &BTreeMap<String, String>,
    ) -> Result<String> {
        let get = |k: &str| sent.get(k).map(String::as_str).unwrap_or("");
        let amount = if c.amount == 0 {
            let digits: String = get("amount").chars().filter(char::is_ascii_digit).collect();
            let n = digits.parse::<i64>().unwrap_or(0);
            if digits.is_empty() || n < c.min || n > c.max {
                return Err(bad(format!(
                    "금액은 {}~{} 사이로 적어 주세요.",
                    won(c.min),
                    won(c.max)
                )));
            }
            n
        } else {
            c.amount
        };
        let buyer_name = text("이름", get("buyer_name"), 1, 60)?;
        let buyer_email = email(get("buyer_email"))?;
        if buyer_email.is_empty() {
            return Err(bad("확인 편지를 받을 전자우편을 적어 주세요."));
        }
        let buyer_phone = if c.ask_phone {
            phone(get("buyer_phone"))?
        } else {
            String::new()
        };
        if get("agree") != "1" {
            return Err(bad(
                "주문 내용과 개인정보 수집에 동의해야 결제할 수 있습니다.",
            ));
        }
        if self.recent_button_orders().await? >= PER_MINUTE {
            return Err(module_error(
                Code::RateLimited,
                "주문이 몰립니다. 잠시 뒤 다시 눌러 주세요.",
            ));
        }
        let req = CheckoutRequest {
            owner: MANIFEST.id.into(),
            ref_id: format!("button:{}", c.key),
            name: c.name.clone(),
            amount,
            buyer_name,
            buyer_email,
            buyer_phone,
            member_id: viewer.member_id().cloned(),
            return_to: c.return_to.clone(),
            expires_minutes: BUTTON_EXPIRES_MINUTES,
        };
        let started = self.create(ctx, &Actor::anonymous([]), req).await?;
        let base = ctx.site_url.trim_end_matches('/');
        Ok(started
            .pay_url
            .strip_prefix(base)
            .unwrap_or(&started.pay_url)
            .to_owned())
    }
}

pub async fn submit(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PayModule>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    Form(sent): Form<BTreeMap<String, String>>,
) -> Response {
    let ctx = &state.ctx;
    if !state.host.same_origin(&headers) {
        return state
            .host
            .error(&module_error(
                Code::Forbidden,
                "이 사이트에서 보낸 폼이 아닙니다.",
            ))
            .await;
    }
    let c = match module.button(ctx, &key).await {
        Ok(c) => c,
        Err(e) => return state.host.error(&e).await,
    };
    // 사람에게 보이지 않는 칸을 채운 자동 프로그램은 주문을 만들지 않고 처음 화면으로.
    if sent.get("website").is_some_and(|v| !v.is_empty()) {
        return Redirect::to("/").into_response();
    }
    let viewer = state
        .host
        .viewer(&headers)
        .await
        .unwrap_or_else(|_| Actor::anonymous([]));
    match module.button_order(ctx, &c, &viewer, &sent).await {
        Ok(path) => Redirect::to(&path).into_response(),
        Err(e) => {
            let status = match e.code() {
                Code::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                Code::Validation => StatusCode::UNPROCESSABLE_ENTITY,
                _ => StatusCode::CONFLICT,
            };
            let data = json!({ "noindex": true, "b": view(&c, &sent, Some(e.message())) });
            let page = state.host.page("mod/pay/button.html", &c.name, data).await;
            super::no_store((status, page).into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widget(amount: i64) -> ButtonWidget {
        ButtonWidget {
            key: String::new(),
            name: "첫 상담".into(),
            amount,
            min: 0,
            max: 0,
            label: String::new(),
            note: String::new(),
            ask_phone: false,
            policy: String::new(),
            return_to: String::new(),
            anchor: String::new(),
            tone: plain(),
        }
    }

    #[test]
    fn button_config_is_shaped() {
        let c = check(widget(30000)).unwrap();
        assert!(key_ok(&c.key));
        assert_eq!((c.min, c.max), (0, 0));
        let custom = check(widget(0)).unwrap();
        assert_eq!((custom.min, custom.max), (1000, 1_000_000));
        assert!(check(widget(-1)).is_err());
        let mut bad_key = widget(1000);
        bad_key.key = "A B".into();
        assert!(check(bad_key).is_err());
        let mut away = widget(1000);
        away.return_to = "//evil.example".into();
        assert!(check(away).is_err());
        let mut jump = widget(1000);
        jump.anchor = " Pay ".into();
        assert_eq!(check(jump).unwrap().anchor, "pay");
        let mut bad_jump = widget(1000);
        bad_jump.anchor = "결제".into();
        assert!(check(bad_jump).is_err());
    }
}
