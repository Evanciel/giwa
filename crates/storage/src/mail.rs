//! 보낼 편지함 `mail_outbox` (GAP S5, ADR-0051). 쌓기는 회원 쓰기와 같은 트랜잭션, 배달은 서버가 따로 한다.

use giwa_core::model::{MailOut, MailStatus};
use giwa_core::{Error, Result};
use sqlx::{Sqlite, SqliteExecutor, Transaction};

use crate::sqlite::db_error;

struct Row {
    id: String,
    idem_key: String,
    to_addr: String,
    subject: String,
    body: String,
    status: String,
    attempts: i64,
    last_error: Option<String>,
    created_at: String,
    sent_at: Option<String>,
}

impl TryFrom<Row> for MailOut {
    type Error = Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(MailOut {
            status: MailStatus::parse(&r.status)
                .ok_or_else(|| Error::internal(format!("알 수 없는 편지 상태: {}", r.status)))?,
            id: r.id,
            idem_key: r.idem_key,
            to_addr: r.to_addr,
            subject: r.subject,
            body: r.body,
            attempts: r.attempts,
            last_error: r.last_error,
            created_at: r.created_at,
            sent_at: r.sent_at,
        })
    }
}

pub async fn queue(tx: &mut Transaction<'static, Sqlite>, m: &MailOut) -> Result<bool> {
    // 탈퇴·휴면 회원의 자리표시 주소에는 쌓지 않는다 (ADR-0073). 모듈이 만든 편지도 여기를 지난다.
    if m.to_addr.ends_with("@invalid") {
        return Ok(false);
    }
    let status = m.status.as_str();
    let done = sqlx::query!(
        "INSERT INTO mail_outbox (id, idem_key, to_addr, subject, body, status, attempts,
                                  last_error, created_at, sent_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (idem_key) DO NOTHING",
        m.id,
        m.idem_key,
        m.to_addr,
        m.subject,
        m.body,
        status,
        m.attempts,
        m.last_error,
        m.created_at,
        m.sent_at
    )
    .execute(&mut **tx)
    .await
    .map_err(db_error)?;
    Ok(done.rows_affected() == 1)
}

pub(crate) async fn update<'e>(e: impl SqliteExecutor<'e>, m: &MailOut) -> Result<()> {
    let status = m.status.as_str();
    sqlx::query!(
        "UPDATE mail_outbox SET status = ?, attempts = ?, last_error = ?, sent_at = ? WHERE id = ?",
        status,
        m.attempts,
        m.last_error,
        m.sent_at,
        m.id
    )
    .execute(e)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn list<'e>(
    e: impl SqliteExecutor<'e>,
    status: Option<MailStatus>,
    limit: i64,
) -> Result<Vec<MailOut>> {
    let status = status.map(MailStatus::as_str);
    sqlx::query_as!(
        Row,
        "SELECT id, idem_key, to_addr, subject, body, status, attempts, last_error, created_at, sent_at
         FROM mail_outbox WHERE (?1 IS NULL OR status = ?1)
         ORDER BY created_at DESC, id DESC LIMIT ?2",
        status,
        limit
    )
    .fetch_all(e)
    .await
    .map_err(db_error)?
    .into_iter()
    .map(MailOut::try_from)
    .collect()
}

pub(crate) async fn by_id<'e>(e: impl SqliteExecutor<'e>, id: &str) -> Result<Option<MailOut>> {
    sqlx::query_as!(
        Row,
        "SELECT id, idem_key, to_addr, subject, body, status, attempts, last_error, created_at, sent_at
         FROM mail_outbox WHERE id = ?",
        id
    )
    .fetch_optional(e)
    .await
    .map_err(db_error)?
    .map(MailOut::try_from)
    .transpose()
}
