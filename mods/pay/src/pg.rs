//! 결제사 어댑터가 함께 쓰는 값 (ADR-0171). 결제사마다 다른 상태 이름을 여기 [`State`] 로 옮겨 주문을 맞춘다.

/// 결제사가 알려 준 결제 하나. 필요한 칸만 읽는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    /// 결제사 결제 키 (토스 paymentKey, PortOne paymentId)
    pub payment_key: String,
    /// 우리가 매긴 결제사 주문 번호 (`<주문>-<시도>`)
    pub order_id: String,
    pub state: State,
    pub total: i64,
    /// 취소하고 남은 금액
    pub balance: i64,
    pub method: String,
    /// 가상계좌 입금 알림 확인 값 (토스)
    pub secret: Option<String>,
}

/// 주문을 맞출 때 보는 결제 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Paid,
    WaitingDeposit,
    Cancelled,
    PartialCancelled,
    Expired,
    /// 준비·진행 중·실패처럼 주문을 바꾸지 않는 상태
    Other,
}

/// 결제사 응답. 닿지 못했으면 `Unreachable`, 거절했으면 `Refused`.
#[derive(Debug)]
pub enum Answer {
    Ok(Payment),
    Refused { code: String, text: String },
    Unreachable(String),
}

/// 시험용으로 결제사 주소를 바꾼다. 이 컴퓨터 안의 http 주소만 받는다(운영에서 돈을 엉뚱한 곳에 묻지 않게).
pub fn api_base(env: &str, default: &str) -> String {
    match std::env::var(env) {
        Ok(v)
            if v.starts_with("http://127.0.0.1:")
                || v.starts_with("http://localhost:")
                || v.starts_with("http://[::1]:") =>
        {
            v.trim_end_matches('/').to_owned()
        }
        _ => default.to_owned(),
    }
}

/// 결제 키를 주소 한 마디로. 결제사 값이지만 웹훅 본문에서 오므로 주소를 바꾸는 글자는 뺀다.
pub fn path_part(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_key_cannot_change_the_path() {
        assert_eq!(path_part("../v1/x?y=1"), "v1xy1");
    }

    #[test]
    fn only_local_test_servers_replace_the_api() {
        assert_eq!(
            api_base("MARU_PAY_TEST_UNSET_ENV", "https://api.example.com"),
            "https://api.example.com"
        );
    }
}
