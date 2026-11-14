//! 주문 편지 (ADR-0170). 상태를 바꾸는 트랜잭션 안에서 쌓는다: 상태가 한 번 바뀌면 편지도 한 번이다.
//! 결제 완료 편지는 결제 모듈도 따로 보낸다(영수증). 이 편지는 상품·배송지·배송 상태를 알린다.

use giwa_core::model::{BusinessInfo, MailOut, MailStatus, SiteSettings};
use giwa_core::{Ctx, Result, clock};

use super::{HOME, ItemRow, OrderRow, ShopModule, Tx, won};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Paid,
    Shipped,
    Delivered,
    Canceled,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Paid => "paid",
            Kind::Shipped => "shipped",
            Kind::Delivered => "delivered",
            Kind::Canceled => "canceled",
        }
    }
}

/// 편지 제목과 본문.
pub fn compose(
    site: &str,
    site_url: &str,
    business: &BusinessInfo,
    kind: Kind,
    o: &OrderRow,
    items: &[ItemRow],
) -> (String, String) {
    let mut lines = vec![format!("{}님, 안녕하세요.", o.name.trim()), String::new()];
    let subject = match kind {
        Kind::Paid => {
            lines.push("주문이 접수됐습니다. 상품을 준비해 보내 드리겠습니다.".into());
            if o.short != 0 {
                lines.push(
                    "죄송합니다. 그 사이 재고가 모자라게 됐습니다. 확인한 뒤 따로 연락드리겠습니다."
                        .into(),
                );
            }
            format!("[{site}] 주문 접수: {}", o.id)
        }
        Kind::Shipped => {
            lines.push("상품을 보냈습니다.".into());
            lines.push(format!("택배사: {}", o.carrier));
            lines.push(format!("운송장 번호: {}", o.tracking));
            format!("[{site}] 상품 발송: {}", o.id)
        }
        Kind::Delivered => {
            lines.push("배송이 끝났습니다. 이용해 주셔서 고맙습니다.".into());
            format!("[{site}] 배송 완료: {}", o.id)
        }
        Kind::Canceled => {
            lines.push("죄송합니다. 주문이 취소됐습니다.".into());
            if !o.cancel_reason.is_empty() {
                lines.push(format!("사유: {}", o.cancel_reason));
            }
            if o.paid_at.is_some() {
                lines.push("결제한 금액은 확인한 뒤 돌려드립니다.".into());
            }
            format!("[{site}] 주문 취소 안내: {}", o.id)
        }
    };
    lines.push(String::new());
    for it in items {
        lines.push(it.line_text());
    }
    let ship = if o.shipping > 0 {
        won(o.shipping)
    } else {
        "무료".to_owned()
    };
    lines.push(format!("배송비: {ship}"));
    lines.push(format!("합계: {}", won(o.total)));
    lines.push(String::new());
    let addr = [o.zip.as_str(), o.address.as_str(), o.address2.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(format!("받는 분: {} / {addr}", o.name.trim()));
    lines.push(format!("주문 번호: {}", o.id));
    lines.push(format!(
        "주문 조회: {}{HOME}/lookup (주문 번호와 이 전자우편 주소로 봅니다)",
        site_url.trim_end_matches('/')
    ));
    if !business.name.is_empty() {
        lines.push(String::new());
        let mut seller = vec![business.name.clone()];
        for v in [&business.phone, &business.email] {
            if !v.is_empty() {
                seller.push(v.clone());
            }
        }
        lines.push(seller.join(" · "));
        if !business.address.is_empty() {
            lines.push(business.address.clone());
        }
    }
    (subject, lines.join("\n"))
}

impl ShopModule {
    /// 구매자 편지를 쌓는다. 전자우편이 없으면 아무 일도 없다.
    pub(super) async fn letter(
        &self,
        tx: &mut Tx,
        ctx: &Ctx,
        site: &SiteSettings,
        kind: Kind,
        o: &OrderRow,
        items: &[ItemRow],
    ) -> Result<()> {
        if o.email.is_empty() {
            return Ok(());
        }
        let (subject, body) = compose(&site.name, &ctx.site_url, &site.business, kind, o, items);
        let out = MailOut {
            id: clock::new_id(),
            idem_key: format!("shop:{}:{}", o.id, kind.key()),
            to_addr: o.email.clone(),
            subject,
            body,
            status: MailStatus::Queued,
            attempts: 0,
            last_error: None,
            created_at: clock::now(),
            sent_at: None,
        };
        giwa_storage::mail::queue(tx, &out).await?;
        Ok(())
    }
}
