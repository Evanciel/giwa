//! 토스페이먼츠 어댑터 (ADR-0166). 2026-11-12 공식 문서·공식 예제(tosspayments-sample)로 확인한 것만 쓴다.
//!
//! - 결제창형 결제위젯: 브라우저 키 `test_gck_`/`live_gck_`, 서버 키 `test_gsk_`/`live_gsk_` 한 쌍.
//! - 인증 `Authorization: Basic base64("{시크릿키}:")`, 승인·취소는 `Idempotency-Key`(≤300자, 15일).
//! - 승인 `POST /v1/payments/confirm`, 조회 `GET /v1/payments/{paymentKey}`,
//!   취소 `POST /v1/payments/{paymentKey}/cancel`.

use maru_storage::fetch::{PayClient, basic_auth};
use serde_json::{Value, json};

const API: &str = "https://api.tosspayments.com";
/// 시험용으로 결제사 주소를 바꾼다. 이 컴퓨터 안의 http 주소만 받는다(운영에서 돈을 엉뚱한 곳에 묻지 않게).
const API_ENV: &str = "MARU_PAY_TOSS_API";

/// 브라우저 SDK 주소 (공식 예제).
pub const SDK: &str = "https://js.tosspayments.com/v2/standard";

pub fn api_base() -> String {
    match std::env::var(API_ENV) {
        Ok(v)
            if v.starts_with("http://127.0.0.1:")
                || v.starts_with("http://localhost:")
                || v.starts_with("http://[::1]:") =>
        {
            v.trim_end_matches('/').to_owned()
        }
        _ => API.to_owned(),
    }
}

/// 키 한 쌍이 맞는지 본다. `live` 인지 돌려준다.
pub fn check_keys(client_key: &str, secret_key: &str) -> Result<bool, String> {
    let ok = |k: &str| {
        !k.is_empty() && k.len() <= 200 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if !ok(client_key) || !ok(secret_key) {
        return Err("키는 영문·숫자·밑줄로 된 200자 이하 값입니다.".into());
    }
    let live = match (client_key.split('_').next(), secret_key.split('_').next()) {
        (Some("test"), Some("test")) => false,
        (Some("live"), Some("live")) => true,
        _ => {
            return Err(
                "클라이언트 키와 시크릿 키가 둘 다 test_ 이거나 둘 다 live_ 여야 합니다.".into(),
            );
        }
    };
    let family = |k: &str| k.split('_').nth(1).unwrap_or("").to_owned();
    if family(client_key) != "gck" || family(secret_key) != "gsk" {
        return Err(
            "결제위젯 연동 키(클라이언트 키 test_gck_…/live_gck_…, 시크릿 키 test_gsk_…/live_gsk_…)를 넣으세요. \
             API 개별 연동 키(ck/sk)는 아직 받지 않습니다."
                .into(),
        );
    }
    Ok(live)
}

/// 결제사가 알려 준 결제 하나. 필요한 칸만 읽는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    pub payment_key: String,
    pub order_id: String,
    /// READY, IN_PROGRESS, WAITING_FOR_DEPOSIT, DONE, CANCELED, PARTIAL_CANCELED, ABORTED, EXPIRED
    pub status: String,
    pub total: i64,
    /// 취소하고 남은 금액
    pub balance: i64,
    pub method: String,
    pub secret: Option<String>,
}

pub fn parse_payment(v: &Value) -> Option<Payment> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let total = v.get("totalAmount").and_then(Value::as_i64)?;
    Some(Payment {
        payment_key: s("paymentKey")?,
        order_id: s("orderId")?,
        status: s("status")?,
        total,
        balance: v
            .get("balanceAmount")
            .and_then(Value::as_i64)
            .unwrap_or(total),
        method: s("method").unwrap_or_default(),
        secret: s("secret").filter(|x| !x.is_empty()),
    })
}

/// 결제사 오류 응답의 문장. 사람이 읽을 수 있게 코드와 함께.
fn refusal(status: u16, v: &Value) -> String {
    let code = v.get("code").and_then(Value::as_str).unwrap_or("");
    let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
    let text: String = format!("{msg} ({code}, 응답 {status})")
        .chars()
        .take(300)
        .collect();
    text
}

/// 결제사 응답. 닿지 못했으면 `Unreachable`, 거절했으면 `Refused`.
#[derive(Debug)]
pub enum Answer {
    Ok(Payment),
    Refused { code: String, text: String },
    Unreachable(String),
}

fn answer(res: Result<(u16, Value), String>) -> Answer {
    match res {
        Err(e) => Answer::Unreachable(e),
        Ok((200, v)) => match parse_payment(&v) {
            Some(p) => Answer::Ok(p),
            None => Answer::Unreachable("결제사 응답을 읽지 못했습니다".into()),
        },
        Ok((status, v)) if status >= 500 => Answer::Unreachable(refusal(status, &v)),
        Ok((status, v)) => Answer::Refused {
            code: v
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            text: refusal(status, &v),
        },
    }
}

pub async fn confirm(secret_key: &str, payment_key: &str, order_id: &str, amount: i64) -> Answer {
    let client = match PayClient::new() {
        Ok(c) => c,
        Err(e) => return Answer::Unreachable(e),
    };
    let body = json!({ "paymentKey": payment_key, "orderId": order_id, "amount": amount });
    answer(
        client
            .post_json(
                &format!("{}/v1/payments/confirm", api_base()),
                &basic_auth(secret_key),
                Some(order_id),
                &body,
            )
            .await,
    )
}

pub async fn lookup(secret_key: &str, payment_key: &str) -> Answer {
    let client = match PayClient::new() {
        Ok(c) => c,
        Err(e) => return Answer::Unreachable(e),
    };
    answer(
        client
            .get_json(
                &format!("{}/v1/payments/{}", api_base(), path_part(payment_key)),
                &basic_auth(secret_key),
            )
            .await,
    )
}

/// 취소(환불). `amount` 가 결제 잔액과 같으면 전액 취소로 보낸다.
pub async fn cancel(
    secret_key: &str,
    payment_key: &str,
    reason: &str,
    amount: Option<i64>,
    idempotency: &str,
) -> Answer {
    let client = match PayClient::new() {
        Ok(c) => c,
        Err(e) => return Answer::Unreachable(e),
    };
    let mut body = json!({ "cancelReason": reason });
    if let Some(a) = amount {
        body["cancelAmount"] = json!(a);
    }
    answer(
        client
            .post_json(
                &format!(
                    "{}/v1/payments/{}/cancel",
                    api_base(),
                    path_part(payment_key)
                ),
                &basic_auth(secret_key),
                Some(idempotency),
                &body,
            )
            .await,
    )
}

/// paymentKey 를 주소 한 마디로. 결제사 값이지만 웹훅 본문에서 오므로 주소를 바꾸는 글자는 뺀다.
fn path_part(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_must_be_a_matching_widget_pair() {
        assert_eq!(check_keys("test_gck_abc", "test_gsk_def"), Ok(false));
        assert_eq!(check_keys("live_gck_abc", "live_gsk_def"), Ok(true));
        assert!(check_keys("test_gck_abc", "live_gsk_def").is_err());
        assert!(check_keys("test_ck_abc", "test_sk_def").is_err());
        assert!(check_keys("test_gck_a/b", "test_gsk_def").is_err());
        assert!(check_keys("", "test_gsk_def").is_err());
    }

    #[test]
    fn reads_payment_and_defaults_balance() {
        let p = parse_payment(&json!({
            "paymentKey": "pk1", "orderId": "o-1", "status": "DONE",
            "totalAmount": 5000, "method": "카드"
        }))
        .unwrap();
        assert_eq!(p.balance, 5000);
        assert_eq!(p.secret, None);
        assert!(parse_payment(&json!({ "paymentKey": "pk1" })).is_none());
    }

    #[test]
    fn payment_key_cannot_change_the_path() {
        assert_eq!(path_part("../v1/x?y=1"), "v1xy1");
    }
}
