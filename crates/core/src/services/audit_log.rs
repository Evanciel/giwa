//! 감사 로그 보기 (GAP S14, ADR-0062). 쓰기는 `store::Tx::audit` 가 하고, 여기서는 읽기만 한다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{AuditEntry, Listing, PageReq};
use crate::store::{AuditFilter, Ctx};

/// 행위자 종류 (`audit_log.actor_type`).
pub const ACTOR_TYPES: [&str; 5] = ["anonymous", "human", "agent", "local", "system"];
/// 한 쪽 기본 개수.
pub const PER_PAGE: u32 = 50;

/// 조회 조건. 비운 칸은 따지지 않는다.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    /// anonymous, human, agent, local, system
    #[serde(default)]
    pub actor_type: Option<String>,
    /// 회원 ID 또는 토큰 ID
    #[serde(default)]
    pub actor_id: Option<String>,
    /// 동작 이름 또는 앞부분 (`post` 면 post.create·post.publish …)
    #[serde(default)]
    pub action: Option<String>,
    /// post, board, page, member, token, site …
    #[serde(default)]
    pub target_type: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    /// 이 시각부터 (RFC3339)
    #[serde(default)]
    pub since: Option<String>,
    /// 이 시각 전까지 (RFC3339)
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub page: Option<u32>,
    /// 1~100, 기본 50
    #[serde(default)]
    pub per_page: Option<u32>,
}

/// 최신 순 감사 기록. 운영자(`site.admin`)만 본다. diff 는 쓸 때 이미 비밀 값이 지워졌다.
pub async fn list(ctx: &Ctx, actor: &Actor, q: &AuditQuery) -> Result<Listing<AuditEntry>> {
    actor.require(Permission::SiteAdmin)?;
    let text = |v: &Option<String>| v.as_deref().map(str::trim).unwrap_or_default().to_owned();
    let actor_type = text(&q.actor_type);
    if !actor_type.is_empty() && !ACTOR_TYPES.contains(&actor_type.as_str()) {
        return Err(Error::new(Code::Validation)
            .with_message("error.audit_actor_type")
            .arg("value", actor_type.chars().take(20).collect::<String>())
            .arg("allowed", ACTOR_TYPES.join(", ")));
    }
    let action = text(&q.action).trim_end_matches('.').to_owned();
    let name_ok = |s: &str| {
        s.len() <= 80
            && s.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
    };
    let target_type = text(&q.target_type);
    for (field, v) in [("action", &action), ("target_type", &target_type)] {
        if !name_ok(v) {
            return Err(Error::new(Code::Validation)
                .with_message("error.audit_name")
                .arg("field", field)
                .arg("value", v.chars().take(40).collect::<String>()));
        }
    }
    let time = |field: &str, v: &Option<String>| -> Result<String> {
        let t = text(v);
        if t.is_empty() {
            return Ok(t);
        }
        clock::parse(&t).map(clock::format).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.audit_time")
                .arg("field", field.to_owned())
                .arg("value", t.chars().take(40).collect::<String>())
        })
    };
    let filter = AuditFilter {
        actor_type,
        actor_id: text(&q.actor_id),
        action,
        target_type,
        target_id: text(&q.target_id),
        since: time("since", &q.since)?,
        until: time("until", &q.until)?,
    };
    let page = PageReq::new(q.page.unwrap_or(1), q.per_page.unwrap_or(PER_PAGE));
    ctx.store.list_audit(&filter, page).await
}
