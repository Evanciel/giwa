//! 결제 확인 편지 (ADR-0167). 감사 기록 `pay.paid`·`pay.awaiting_deposit`·`pay.refunded` 를 받아
//! 구매자에게 보낸다. 배달은 최소 한 번이라 편지 묶음 키를 감사 기록 ID 로 잡아 한 번만 쌓는다.
//!
//! 주문 화면 주소의 1회용 값은 해시만 남으므로 편지에는 싣지 않는다. 주문 번호와 판매자 연락처를 적는다.

use giwa_core::model::{AuditEvent, BusinessInfo, MailOut, MailStatus};
use giwa_core::services::settings;
use giwa_core::{Ctx, Error, Result, clock};

use super::{AWAITING, OrderRow, PAID, PayModule, local_time, won};

/// 편지 제목과 본문. 보낼 것이 없으면 `None`.
pub fn compose(
    site: &str,
    business: &BusinessInfo,
    tz: i32,
    action: &str,
    o: &OrderRow,
    bank: Option<(String, String, String)>,
) -> Option<(String, String)> {
    let mut lines = vec![
        format!("{}님, 안녕하세요.", o.buyer_name.trim()),
        String::new(),
    ];
    let subject = match action {
        "pay.paid" if o.status == PAID || o.paid_at.is_some() => {
            lines.push("결제가 끝났습니다.".into());
            format!("[{site}] 결제 완료: {}", o.name)
        }
        "pay.awaiting_deposit" if o.status == AWAITING => {
            lines.push("주문을 받았습니다. 입금하면 결제가 끝납니다.".into());
            // 토스 가상계좌는 결제사가 계좌를 알려 주므로 우리 계좌를 적지 않는다.
            if let (Some((bank, account, holder)), "bank") = (&bank, o.provider.as_str()) {
                lines.push(format!("입금 계좌: {bank} {account} ({holder})"));
            }
            if !o.depositor.is_empty() {
                lines.push(format!("입금자 이름: {}", o.depositor));
            }
            lines.push(format!("입금 기한: {}", local_time(tz, &o.expires_at)));
            format!("[{site}] 입금 안내: {}", o.name)
        }
        "pay.refunded" => {
            lines.push(format!("{}을 환불했습니다.", won(o.refunded)));
            if o.provider == "toss" {
                lines.push("카드사 사정에 따라 돌아가기까지 며칠 걸릴 수 있습니다.".into());
            }
            format!("[{site}] 환불 안내: {}", o.name)
        }
        _ => return None,
    };
    lines.push(String::new());
    lines.push(format!("주문: {}", o.name));
    lines.push(format!("금액: {}", won(o.amount)));
    if !o.method.is_empty() {
        lines.push(format!("결제 방법: {}", o.method));
    }
    lines.push(format!("주문 번호: {}", o.id));
    lines.push(String::new());
    lines.push("문의할 때 주문 번호를 알려 주세요.".into());
    if !business.name.is_empty() {
        lines.push(String::new());
        let mut seller = vec![business.name.clone()];
        for v in [&business.phone, &business.email] {
            if !v.is_empty() {
                seller.push(v.clone());
            }
        }
        lines.push(seller.join(" · "));
        if !business.reg_no.is_empty() {
            lines.push(format!("사업자등록번호 {}", business.reg_no));
        }
    }
    Some((subject, lines.join("\n")))
}

impl PayModule {
    pub(super) async fn send_letter(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        let Some(id) = &ev.target_id else {
            return Ok(());
        };
        let Some(o) = self.order_row(id).await? else {
            return Ok(());
        };
        if o.buyer_email.is_empty() {
            return Ok(());
        }
        let site = settings::site(ctx).await?;
        let bank = self
            .providers()
            .await?
            .into_iter()
            .find(|p| p.id == "bank" && p.usable())
            .map(|p| (p.conf("bank"), p.conf("account"), p.conf("holder")));
        let Some((subject, body)) = compose(
            &site.name,
            &site.business,
            site.tz_offset_min,
            &ev.action,
            &o,
            bank,
        ) else {
            return Ok(());
        };
        let out = MailOut {
            id: clock::new_id(),
            idem_key: format!("pay:{}", ev.id),
            to_addr: o.buyer_email.clone(),
            subject,
            body,
            status: MailStatus::Queued,
            attempts: 0,
            last_error: None,
            created_at: clock::now(),
            sent_at: None,
        };
        let mut tx = self
            .db
            .write_pool()
            .begin()
            .await
            .map_err(Error::internal)?;
        giwa_storage::mail::queue(&mut tx, &out).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }
}
