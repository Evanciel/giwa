//! 다른 모듈이 결제를 맡기는 길 (ADR-0166).
//!
//! 예약·쇼핑 같은 모듈은 결제 모듈 크레이트를 모르고 이 트레이트만 안다. 바이너리가 결제 모듈을 만들어
//! 필요한 모듈에 넘긴다. 결제가 끝나거나 환불되면 결제 모듈이 감사 기록 `pay.paid`·`pay.refunded` 를 남기고,
//! 맡긴 모듈은 그 이벤트(`target` = 주문, 내용의 `owner`·`ref_id`)를 받아 자기 상태를 바꾼다.
//! 배달은 최소 한 번이므로 받는 쪽이 멱등해야 한다.

use async_trait::async_trait;
use maru_core::{Ctx, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 주문 만들기. 금액은 맡기는 모듈이 자기 데이터(가격·예약금)로 계산한 값이다. 브라우저 값을 넣지 않는다.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct CheckoutRequest {
    /// 맡기는 모듈 id. 이벤트를 받을 때 자기 주문인지 가린다
    pub owner: String,
    /// 맡기는 모듈 안의 대상 (예약 ID, 장바구니 ID)
    pub ref_id: String,
    /// 결제 화면과 결제사 창에 보일 주문 이름
    pub name: String,
    /// 원 단위 정수
    pub amount: i64,
    pub buyer_name: String,
    pub buyer_email: String,
    pub buyer_phone: String,
    pub member_id: Option<String>,
    /// 결제가 끝나면 보낼 사이트 안 주소. 비우면 결제 모듈의 주문 화면
    pub return_to: String,
    /// 이 시간(분)이 지나도록 결제하지 않으면 주문이 끝난다. 0 이면 결제 모듈 기본값
    pub expires_minutes: i64,
}

/// 주문 상태. 구매자 정보는 싣지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CheckoutOrder {
    pub id: String,
    pub owner: String,
    pub ref_id: String,
    pub name: String,
    pub amount: i64,
    /// 지금까지 환불한 금액
    pub refunded: i64,
    /// `pending`, `awaiting_deposit`, `paid`, `partial_refunded`, `refunded`, `expired`
    pub status: String,
    /// 결제한 길 (`bank`, `toss`). 아직 안 골랐으면 빈 값
    pub provider: String,
    pub created_at: String,
    pub expires_at: String,
    pub paid_at: Option<String>,
}

/// 만든 주문과 구매자를 보낼 결제 화면 주소(1회용 값이 들어 있어 다시 만들 수 없다).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckoutStarted {
    pub order: CheckoutOrder,
    pub pay_url: String,
}

#[async_trait]
pub trait Checkout: Send + Sync {
    /// 주문을 만든다. 결제 모듈이 꺼져 있거나 쓸 수 있는 결제 방법이 없으면 오류.
    async fn create_order(&self, ctx: &Ctx, req: CheckoutRequest) -> Result<CheckoutStarted>;
    /// 주문 상태. 없으면 `None`.
    async fn order(&self, ctx: &Ctx, id: &str) -> Result<Option<CheckoutOrder>>;
}
