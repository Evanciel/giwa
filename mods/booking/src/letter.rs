//! 예약 편지 (ADR-0169). 상태를 바꾸는 트랜잭션 안에서 쌓는다: 상태가 한 번 바뀌면 편지도 한 번이다.
//!
//! 손님 취소 링크의 1회용 값은 해시만 남는다. 그래서 확정·접수 편지를 쌓을 때마다 새 값을 만들어
//! 편지에만 싣고, 앞서 보낸 링크는 쓸 수 없게 된다(가장 최근 편지의 링크가 살아 있다).

use giwa_core::model::{BusinessInfo, MailOut, MailStatus, SiteSettings};
use giwa_core::{Ctx, Result, clock};

use super::{BookingModule, BookingRow, Conf, HOME, won};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Requested,
    Confirmed,
    CanceledByGuest,
    CanceledByOperator,
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Requested => "requested",
            Kind::Confirmed => "confirmed",
            Kind::CanceledByGuest => "canceled_guest",
            Kind::CanceledByOperator => "canceled",
        }
    }
}

/// 편지 제목과 본문.
pub fn compose(
    site: &str,
    business: &BusinessInfo,
    cancel_hours: i64,
    kind: Kind,
    b: &BookingRow,
    cancel_url: Option<&str>,
) -> (String, String) {
    let mut lines = vec![format!("{}님, 안녕하세요.", b.name.trim()), String::new()];
    let subject = match kind {
        Kind::Requested => {
            lines.push("예약 신청을 받았습니다. 확인한 뒤 확정 편지를 보내 드립니다.".into());
            format!("[{site}] 예약 신청: {}", b.service_name)
        }
        Kind::Confirmed => {
            lines.push("예약이 확정됐습니다.".into());
            format!("[{site}] 예약 확정: {}", b.service_name)
        }
        Kind::CanceledByGuest => {
            lines.push("예약을 취소했습니다.".into());
            format!("[{site}] 예약 취소: {}", b.service_name)
        }
        Kind::CanceledByOperator => {
            lines.push("죄송합니다. 예약이 취소됐습니다.".into());
            if !b.cancel_reason.is_empty() {
                lines.push(format!("사유: {}", b.cancel_reason));
            }
            format!("[{site}] 예약 취소 안내: {}", b.service_name)
        }
    };
    lines.push(String::new());
    lines.push(format!("예약: {}", b.service_name));
    lines.push(format!("일시: {}", b.when()));
    if b.deposit > 0 {
        let state = if b.paid != 0 {
            "결제함"
        } else {
            "결제 전"
        };
        lines.push(format!("예약금: {} ({state})", won(b.deposit)));
    }
    lines.push(format!("예약 번호: {}", b.id));
    let canceled = matches!(kind, Kind::CanceledByGuest | Kind::CanceledByOperator);
    if canceled && b.paid != 0 {
        lines.push(String::new());
        lines.push("받은 예약금은 확인한 뒤 돌려드립니다.".into());
    }
    if let Some(url) = cancel_url {
        lines.push(String::new());
        lines.push(format!(
            "예약 {cancel_hours}시간 전까지 아래 주소에서 직접 취소할 수 있습니다. 주소는 한 번만 쓸 수 있으니 다른 사람에게 보내지 마세요."
        ));
        lines.push(url.to_owned());
    }
    lines.push(String::new());
    lines.push("문의할 때 예약 번호를 알려 주세요.".into());
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

impl BookingModule {
    /// 손님 편지를 쌓는다. 전자우편이 없으면 아무 일도 없다.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn letter(
        &self,
        tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
        ctx: &Ctx,
        site: &SiteSettings,
        conf: &Conf,
        kind: Kind,
        b: &BookingRow,
        key: Option<&str>,
    ) -> Result<()> {
        if b.email.is_empty() {
            return Ok(());
        }
        let url = key.map(|k| {
            format!(
                "{}{HOME}/c/{}?k={k}",
                ctx.site_url.trim_end_matches('/'),
                b.id
            )
        });
        let (subject, body) = compose(
            &site.name,
            &site.business,
            conf.cancel_hours,
            kind,
            b,
            url.as_deref(),
        );
        let out = MailOut {
            id: clock::new_id(),
            idem_key: format!("booking:{}:{}", b.id, kind.key()),
            to_addr: b.email.clone(),
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
