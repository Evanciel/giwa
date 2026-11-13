//! PortOne(포트원) V2 어댑터 (ADR-0171). 2026-11-14 공식 개발자 문서(developers.portone.io)로 확인한 것만 쓴다.
//!
//! - 브라우저: `PortOne.requestPayment({storeId, channelKey, paymentId, orderName, totalAmount, currency, payMethod,
//!   redirectUrl})`. 모바일은 늘 `redirectUrl` 로 돌아오고(`?paymentId=…`, 실패면 `code`·`message`), 데스크톱은
//!   응답을 돌려준다. 결제 창이 결제사에서 바로 승인하므로 서버 승인 단계가 없다. 그래서 돌아오면 조회로 맞춘다.
//! - 인증 `Authorization: PortOne {API 시크릿}`. 조회 `GET /payments/{paymentId}`,
//!   취소 `POST /payments/{paymentId}/cancel` (`reason`, `amount`, `currentCancellableAmount`).
//! - 웹훅 본문 `{type: "Transaction.Paid" …, data: {paymentId, storeId}}` 은 믿지 않고 조회로 다시 묻는다.
//!   그래서 웹훅 서명 시크릿은 받지 않는다.

use crate::pg::{Answer, Payment, State, path_part};
use maru_storage::fetch::PayClient;
use serde_json::{Value, json};

const API: &str = "https://api.portone.io";
/// 시험용으로 결제사 주소를 바꾼다 (`pg::api_base`).
const API_ENV: &str = "MARU_PAY_PORTONE_API";

/// 브라우저 SDK 주소 (공식 문서).
pub const SDK: &str = "https://cdn.portone.io/v2/browser-sdk.js";

pub fn api_base() -> String {
    crate::pg::api_base(API_ENV, API)
}

fn auth(secret: &str) -> String {
    format!("PortOne {secret}")
}

/// 상점 아이디·채널 키·API 시크릿 모양을 본다. PortOne 키에는 시험·운영 표시가 없어 운영 여부는 따로 받는다.
pub fn check_keys(store_id: &str, channel_key: &str, secret: &str) -> Result<(), String> {
    let id = |k: &str, prefix: &str| {
        k.starts_with(prefix)
            && k.len() <= 100
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if !id(store_id, "store-") {
        return Err("상점 아이디(store_id)는 store- 로 시작하는 값입니다. 관리자 콘솔 [결제 연동] 에 있습니다.".into());
    }
    if !id(channel_key, "channel-key-") {
        return Err("채널 키(client_key)는 channel-key- 로 시작하는 값입니다. 관리자 콘솔 [결제 연동 > 채널 관리] 에 있습니다.".into());
    }
    if secret.is_empty() || secret.len() > 300 || !secret.chars().all(|c| c.is_ascii_graphic()) {
        return Err("API 시크릿(secret_key)은 빈칸 없는 300자 이하 값입니다. V1 API 키·시크릿이 아니라 V2 API 시크릿을 넣으세요.".into());
    }
    Ok(())
}

/// PortOne 상태: READY, PENDING, VIRTUAL_ACCOUNT_ISSUED, PAID, FAILED, PARTIAL_CANCELLED, CANCELLED
fn state(s: &str) -> State {
    match s {
        "PAID" => State::Paid,
        "VIRTUAL_ACCOUNT_ISSUED" => State::WaitingDeposit,
        "CANCELLED" => State::Cancelled,
        "PARTIAL_CANCELLED" => State::PartialCancelled,
        _ => State::Other,
    }
}

fn method(v: &Value) -> String {
    match v.pointer("/method/type").and_then(Value::as_str) {
        Some("PaymentMethodCard") => "카드",
        Some("PaymentMethodEasyPay") => "간편결제",
        Some("PaymentMethodTransfer") => "계좌이체",
        Some("PaymentMethodVirtualAccount") => "가상계좌",
        Some("PaymentMethodMobile") => "휴대폰",
        Some("PaymentMethodGiftCertificate") => "상품권",
        _ => "",
    }
    .to_owned()
}

/// 결제 건 하나. 다른 상점의 결제이거나 원화가 아니면 읽지 않는다.
pub fn parse_payment(v: &Value, store_id: &str) -> Option<Payment> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    if s("storeId").as_deref() != Some(store_id) || s("currency").as_deref() != Some("KRW") {
        return None;
    }
    let id = s("id")?;
    let total = v.pointer("/amount/total").and_then(Value::as_i64)?;
    let cancelled = v
        .pointer("/amount/cancelled")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    Some(Payment {
        payment_key: id.clone(),
        order_id: id,
        state: state(&s("status")?),
        total,
        balance: total - cancelled,
        method: method(v),
        secret: None,
    })
}

/// 결제사 오류 응답의 문장. 사람이 읽을 수 있게 종류와 함께.
fn refusal(status: u16, v: &Value) -> String {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
    format!("{msg} ({kind}, 응답 {status})")
        .chars()
        .take(300)
        .collect()
}

pub async fn lookup(store_id: &str, secret: &str, payment_id: &str) -> Answer {
    let client = match PayClient::new() {
        Ok(c) => c,
        Err(e) => return Answer::Unreachable(e),
    };
    let url = format!("{}/payments/{}", api_base(), path_part(payment_id));
    match client.get_json(&url, &auth(secret)).await {
        Err(e) => Answer::Unreachable(e),
        Ok((200, v)) => match parse_payment(&v, store_id) {
            Some(p) => Answer::Ok(p),
            None => Answer::Refused {
                code: "MISMATCH".into(),
                text: "결제사 결제가 이 상점·원화 결제가 아닙니다".into(),
            },
        },
        Ok((status, v)) if status >= 500 => Answer::Unreachable(refusal(status, &v)),
        Ok((status, v)) => Answer::Refused {
            code: v
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            text: refusal(status, &v),
        },
    }
}

/// 취소 결과. 결제사가 접수만 했으면(`Requested`) 끝난 뒤 웹훅·조회로 맞춘다.
#[derive(Debug, PartialEq, Eq)]
pub enum Cancel {
    Done,
    Requested,
}

/// 취소(환불). `cancellable` 은 우리가 아는 남은 금액이다. 결제사 잔액과 다르면 결제사가 거절한다(겹친 환불 방지).
pub async fn cancel(
    secret: &str,
    payment_id: &str,
    reason: &str,
    amount: Option<i64>,
    cancellable: i64,
) -> Result<Cancel, String> {
    let client = PayClient::new()?;
    let mut body = json!({ "reason": reason, "currentCancellableAmount": cancellable });
    if let Some(a) = amount {
        body["amount"] = json!(a);
    }
    let url = format!("{}/payments/{}/cancel", api_base(), path_part(payment_id));
    match client.post_json(&url, &auth(secret), None, &body).await? {
        (200, v) => match v.pointer("/cancellation/status").and_then(Value::as_str) {
            Some("SUCCEEDED") => Ok(Cancel::Done),
            Some("REQUESTED") => Ok(Cancel::Requested),
            Some("FAILED") => Err("결제사가 취소에 실패했습니다".into()),
            _ => Err("결제사 응답을 읽지 못했습니다".into()),
        },
        (status, v) => Err(refusal(status, &v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_have_portone_shapes() {
        assert!(check_keys("store-abc-1", "channel-key-def-2", "secret").is_ok());
        assert!(check_keys("abc", "channel-key-def", "secret").is_err());
        assert!(check_keys("store-abc", "channel-def", "secret").is_err());
        assert!(check_keys("store-abc", "channel-key-d/f", "secret").is_err());
        assert!(check_keys("store-abc", "channel-key-def", "has space").is_err());
        assert!(check_keys("store-abc", "channel-key-def", "").is_err());
    }

    #[test]
    fn reads_only_our_store_in_won() {
        let v = json!({
            "status": "PARTIAL_CANCELLED", "id": "o1-1", "storeId": "store-a", "currency": "KRW",
            "amount": { "total": 5000, "cancelled": 2000 }, "method": { "type": "PaymentMethodCard" }
        });
        let p = parse_payment(&v, "store-a").unwrap();
        assert_eq!(p.state, State::PartialCancelled);
        assert_eq!((p.total, p.balance), (5000, 3000));
        assert_eq!(p.method, "카드");
        assert!(parse_payment(&v, "store-b").is_none());
        let mut usd = v.clone();
        usd["currency"] = json!("USD");
        assert!(parse_payment(&usd, "store-a").is_none());
    }
}
