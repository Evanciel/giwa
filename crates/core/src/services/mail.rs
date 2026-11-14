//! 보낼 편지함과 배달 (GAP S5, ADR-0051).
//!
//! - 편지는 회원 쓰기와 같은 트랜잭션에서 `mail_outbox` 에 쌓는다. 같은 `idem_key` 는 한 번만 쌓인다.
//! - 배달은 서버가 주기적으로(`deliver_pending`) 한다. 실패하면 `MAX_ATTEMPTS` 번까지 다시 하고, 그 뒤엔 `failed` 로 남긴다.
//! - 전송 방식은 `Mailer` 트레이트. 데이터 디렉터리에 .eml 로 남기는 방식(기본)과 SMTP(ADR-0083)가 있다.
//! - 편지 본문에는 1회용 링크가 들어 있어 REST 로 내보내지 않는다. 로컬 CLI 만 읽는다.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

use crate::actor::{Actor, ActorKind};
use crate::audit::Target;
use crate::clock;
use crate::config::looks_like_email;
use crate::error::{Code, Error, Result};
use crate::messages;
use crate::model::{MailOut, MailStatus};
use crate::store::{Ctx, Tx};

pub const MAX_ATTEMPTS: i64 = 5;
/// 한 번에 배달할 편지 수
pub const BATCH: i64 = 20;
/// 같은 사람에게 같은 종류의 편지를 다시 쌓지 않는 칸 (초)
pub const REPEAT_WINDOW_SECS: i64 = 600;

/// 편지를 실제로 내보내는 곳.
#[async_trait::async_trait]
pub trait Mailer: Send + Sync {
    /// `file`, `smtp` …
    fn kind(&self) -> &'static str;
    async fn send(&self, mail: &MailOut) -> std::result::Result<(), String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Delivery {
    pub sent: u32,
    pub retry: u32,
    pub failed: u32,
}

/// 지금 시각이 든 칸 번호. `idem_key` 에 붙여 같은 칸 안의 같은 편지를 하나로 묶는다.
pub fn window() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp() / REPEAT_WINDOW_SECS
}

/// 메시지 키 `mail.<kind>.subject`·`mail.<kind>.body` 로 편지를 만들어 쌓는다. 쌓였으면 `true`.
pub async fn queue(
    tx: &mut Tx,
    idem_key: String,
    to: &str,
    kind: &str,
    args: &BTreeMap<&'static str, String>,
) -> Result<bool> {
    let mail = MailOut {
        id: clock::new_id(),
        idem_key,
        to_addr: to.to_owned(),
        subject: messages::render(&format!("mail.{kind}.subject"), args),
        body: messages::render(&format!("mail.{kind}.body"), args),
        status: MailStatus::Queued,
        attempts: 0,
        last_error: None,
        created_at: clock::now(),
        sent_at: None,
    };
    tx.queue_mail(&mail).await
}

/// 쌓인 편지를 배달한다. 감사 대상이 아니다(편지마다 상태가 남는다).
pub async fn deliver_pending(ctx: &Ctx, mailer: &dyn Mailer) -> Result<Delivery> {
    let mut report = Delivery::default();
    let mut queued = ctx.store.mails(Some(MailStatus::Queued), BATCH).await?;
    queued.reverse(); // 오래된 것부터
    for mut m in queued {
        m.attempts += 1;
        match mailer.send(&m).await {
            Ok(()) => {
                m.status = MailStatus::Sent;
                m.sent_at = Some(clock::now());
                m.last_error = None;
                report.sent += 1;
            }
            Err(e) => {
                m.last_error = Some(e.chars().take(500).collect());
                if m.attempts >= MAX_ATTEMPTS {
                    m.status = MailStatus::Failed;
                    report.failed += 1;
                } else {
                    report.retry += 1;
                }
            }
        }
        ctx.store.update_mail(&m).await?;
    }
    Ok(report)
}

/// 설정을 확인하는 시험 편지를 쌓는다(로컬 운영자만). 돌려주는 값은 편지의 `idem_key`.
/// 같은 주소로 10분 안에 다시 부르면 새로 쌓지 않고 같은 키를 돌려준다.
pub async fn queue_test(ctx: &Ctx, actor: &Actor, to: &str) -> Result<String> {
    require_local(actor)?;
    let to = to.trim();
    if !looks_like_email(to) {
        return Err(Error::new(Code::Validation)
            .with_message("error.mail_to")
            .arg("value", to)
            .next("giwa mail test --to you@example.test"));
    }
    let site = crate::services::settings::site(ctx).await?;
    let key = format!("test:{}:{}", to.to_lowercase(), window());
    let mut tx = Tx::begin(ctx).await?;
    let args = BTreeMap::from([("site", site.name)]);
    if queue(&mut tx, key.clone(), to, "test", &args).await? {
        let domain = to.rsplit('@').next().unwrap_or_default();
        tx.audit(
            actor,
            "mail.test",
            Target::new("mail", "test"),
            json!({ "to_domain": domain }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(key)
}

/// 편지 보내는 방식을 바꾼 일을 감사에 남긴다(설정 파일은 storage 가 쓴다). 비밀번호는 남기지 않는다.
pub async fn record_setup(ctx: &Ctx, actor: &Actor, diff: serde_json::Value) -> Result<()> {
    require_local(actor)?;
    let mut tx = Tx::begin(ctx).await?;
    tx.audit(actor, "mail.setup", Target::new("mail", "config"), diff)
        .await?;
    tx.commit().await
}

/// 편지함 보기. 본문에 1회용 링크가 들어 있어 로컬 운영자(CLI)만 읽는다.
pub async fn list(
    ctx: &Ctx,
    actor: &Actor,
    status: Option<MailStatus>,
    limit: i64,
) -> Result<Vec<MailOut>> {
    require_local(actor)?;
    ctx.store.mails(status, limit.clamp(1, 200)).await
}

pub async fn get(ctx: &Ctx, actor: &Actor, id: &str) -> Result<MailOut> {
    require_local(actor)?;
    ctx.store.mail_by_id(id).await?.ok_or_else(|| {
        Error::new(Code::NotFound)
            .with_message("error.mail_not_found")
            .arg("id", id)
            .next("giwa mail list")
    })
}

fn require_local(actor: &Actor) -> Result<()> {
    match actor.kind() {
        ActorKind::Local => Ok(()),
        _ => Err(Error::new(Code::Forbidden).with_message("error.mail_local_only")),
    }
}
