//! 개인정보 보유기간 (GAP M17, ADR-0073).
//!
//! - 휴면은 켜는 설정이다(`dormant_days`, 0 이면 끔). 2023 개정 개인정보보호법에서 1년 휴면 의무가 없어졌다.
//! - 휴면 `NOTICE_DAYS` 일 전에 편지로 알리고, 알린 뒤 `NOTICE_DAYS` 일이 지나야 휴면으로 바꾼다.
//! - 휴면이면 메일 주소를 `member_dormant` 로 옮기고 세션을 끊는다. 다시 로그인하면
//!   `auth::start_session` 이 되돌린다. 메일로 찾기는 분리 표도 보므로 비밀번호 로그인·재설정이 그대로 된다.
//! - 보냈거나 실패한 편지는 `mail_retention_days` 가 지나면 지운다(본문에 1회용 링크가 있다).
//! - 감사 기록(접속 기록)은 지우지 않는다. 안전성 확보 조치 기준상 1년 이상 남겨야 한다.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::Result;
use crate::services::{mail, settings};
use crate::store::{Ctx, Tx};

/// 휴면 전에 알리는 날수. 알린 뒤에도 이만큼 기다린다.
pub const NOTICE_DAYS: u32 = 30;
pub const MIN_DORMANT_DAYS: u32 = 180;
pub const MIN_MAIL_DAYS: u32 = 7;
pub const MAX_DAYS: u32 = 3650;
const BATCH: i64 = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Report {
    /// 휴면 미리 알림 편지를 쌓은 회원 수
    pub notified: u32,
    /// 휴면으로 바꾼 회원 수
    pub dormant: u32,
    /// 지운 편지 수
    pub mails_removed: u64,
    /// 모듈이 지운 기록 수 (오래된 알림 등)
    pub module_removed: u64,
}

/// 보존 정리를 한 번 한다. 서버가 하루 한 번 부르고, CLI·REST 로 바로 부를 수도 있다. 여러 번 불러도 안전하다.
pub async fn run(ctx: &Ctx, actor: &Actor) -> Result<Report> {
    actor.require(Permission::MemberManage)?;
    let site = settings::site(ctx).await?;
    let mut report = Report {
        mails_removed: ctx
            .store
            .purge_mails(&days_ago(site.mail_retention_days))
            .await?,
        module_removed: ctx.extensions.purge(ctx).await,
        ..Report::default()
    };
    if site.dormant_days == 0 {
        return Ok(report);
    }

    let seen_notice = days_ago(site.dormant_days.saturating_sub(NOTICE_DAYS));
    let link = format!("{}/login", ctx.site_url);
    loop {
        let batch = ctx.store.members_to_notify(&seen_notice, BATCH).await?;
        if batch.is_empty() {
            break;
        }
        let mut tx = Tx::begin(ctx).await?;
        for m in &batch {
            let now = clock::now();
            tx.mark_dormant_notice(&m.id, &now).await?;
            let args = BTreeMap::from([
                ("site", site.name.clone()),
                ("name", m.display_name.clone()),
                ("days", site.dormant_days.to_string()),
                ("link", link.clone()),
            ]);
            mail::queue(
                &mut tx,
                format!("dormant_notice:{}:{now}", m.id),
                &m.email,
                "dormant_notice",
                &args,
            )
            .await?;
            tx.audit(
                actor,
                "member.dormant_notice",
                Target::new("member", &m.id),
                json!({ "days": site.dormant_days }),
            )
            .await?;
        }
        tx.commit().await?;
        report.notified += batch.len() as u32;
        if (batch.len() as i64) < BATCH {
            break;
        }
    }

    let seen = days_ago(site.dormant_days);
    let noticed = days_ago(NOTICE_DAYS);
    loop {
        let batch = ctx.store.members_to_sleep(&seen, &noticed, BATCH).await?;
        if batch.is_empty() {
            break;
        }
        let mut tx = Tx::begin(ctx).await?;
        let mut moved = 0;
        for m in &batch {
            if !tx.make_dormant(&m.id, &clock::now()).await? {
                continue;
            }
            tx.delete_member_sessions(&m.id).await?;
            tx.audit(
                actor,
                "member.dormant",
                Target::new("member", &m.id),
                json!({ "days": site.dormant_days }),
            )
            .await?;
            moved += 1;
        }
        if moved == 0 {
            break;
        }
        tx.commit().await?;
        report.dormant += moved;
        if (batch.len() as i64) < BATCH {
            break;
        }
    }
    Ok(report)
}

fn days_ago(days: u32) -> String {
    clock::hours_ago(i64::from(days) * 24)
}
