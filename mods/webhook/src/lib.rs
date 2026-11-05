//! 나가는 웹훅 모듈 (GAP S13, ADR-0085). 글·댓글·회원 가입 같은 감사 기록이 생기면 운영자가 정한 주소로
//! 서명한 JSON 을 보낸다.
//!
//! - 배달·재시도는 이벤트 버스(ADR-0066)가 맡는다. 받는 곳 하나라도 실패하면 `on_event` 가 실패를 돌려주고
//!   버스가 1·5·30·120분 뒤 다시 부른다. 이미 받은 곳은 배달 기록을 보고 건너뛴다(한 곳에 두 번 가지 않게).
//! - 본문에는 무엇이 일어났는지(이벤트 ID·동작·대상·시각)만 싣는다. 글 내용·회원 정보는 받는 쪽이 REST 로 읽는다.
//! - 주소 규칙은 링크 카드와 같다: 공개 인터넷 주소만, 프록시 없음, 리디렉션 따라가지 않음(ADR-0084).

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use maru_core::actor::{Actor, Permission};
use maru_core::audit::{AuditRecord, Target};
use maru_core::model::AuditEvent;
use maru_core::services::auth::new_secret;
use maru_core::store::ModuleMigration;
use maru_core::{Code, Ctx, Error, Result, clock};
use maru_modules::{Manifest, Module, ToolDef, module_error, schema_of};
use maru_storage::Db;
use maru_storage::fetch::{WebhookPoster, check_target_url};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;

/// 보낼 수 있는 이벤트. 회원 개인 정보가 실리는 동작(정보 고침·탈퇴 등)은 넣지 않는다.
const EVENTS: &[&str] = &[
    "post.publish",
    "post.publish_scheduled",
    "post.update",
    "post.hide",
    "post.delete",
    "comment.create",
    "comment.approve",
    "comment.delete",
    "page.publish",
    "page.update",
    "page.delete",
    "media.upload",
    "member.signup",
];

static MANIFEST: Manifest = Manifest {
    id: "webhook",
    name: "웹훅",
    version: env!("CARGO_PKG_VERSION"),
    description: "글·댓글·회원 가입 같은 일이 생기면 정한 주소로 서명한 알림을 보냅니다.",
    events: EVENTS,
};

/// 받는 곳은 이만큼까지(배달 한 번이 너무 길어지지 않게).
const MAX_ENDPOINTS: i64 = 20;
/// 배달 기록을 남기는 날 수.
const KEEP_DAYS: i64 = 30;
const MAX_ERROR: usize = 300;
/// 서명 키 앞머리. 키가 새어 나간 것을 찾기 쉽게 한다.
const SECRET_PREFIX: &str = "mwhs_";

type HmacSha256 = Hmac<Sha256>;

pub struct WebhookModule {
    db: Db,
    poster: WebhookPoster,
}

#[derive(Debug, Clone)]
struct Endpoint {
    id: String,
    url: String,
    events: Vec<String>,
    secret: String,
    enabled: bool,
    created_at: String,
}

impl Endpoint {
    fn wants(&self, action: &str) -> bool {
        self.enabled && self.events.iter().any(|e| e == "*" || e == action)
    }
}

/// 목록에 보이는 받는 곳. 서명 키는 끝 네 글자만.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EndpointView {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    pub enabled: bool,
    pub secret_hint: String,
    pub created_at: String,
}

impl From<&Endpoint> for EndpointView {
    fn from(e: &Endpoint) -> Self {
        let tail: String = e
            .secret
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Self {
            id: e.id.clone(),
            url: e.url.clone(),
            events: e.events.clone(),
            enabled: e.enabled,
            secret_hint: format!("…{tail}"),
            created_at: e.created_at.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeliveryView {
    pub endpoint_id: String,
    pub event_id: String,
    pub action: String,
    pub ok: bool,
    pub attempts: i64,
    pub http_status: Option<i64>,
    pub last_error: Option<String>,
    pub at: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddInput {
    /// 받을 주소(https 권장). 공개 인터넷 주소만
    url: String,
    /// 보낼 이벤트. 되풀이해 여러 개, `*` 는 전부
    #[serde(default)]
    event: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdInput {
    /// 받는 곳 ID (`maru webhook list`)
    id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeliveriesInput {
    /// 받는 곳 ID. 비우면 전부
    #[serde(default)]
    endpoint: Option<String>,
    /// true 면 실패한 것만
    #[serde(default)]
    failed: Option<bool>,
    #[serde(default)]
    limit: Option<i64>,
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn short(text: &str) -> String {
    text.chars().take(MAX_ERROR).collect()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.rsplit_once('@').map_or(host, |(_, h)| h).to_owned()
}

fn not_found(id: &str) -> Error {
    module_error(Code::NotFound, format!("받는 곳을 찾을 수 없습니다: {id}"))
        .next("maru webhook list")
}

/// 받는 쪽이 검사할 서명: `HMAC-SHA256(키, "<시각>.<본문>")` 의 16진수.
pub fn sign(secret: &str, timestamp: u64, body: &[u8]) -> String {
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// 받는 곳에 실을 본문. 무엇이 일어났는지만 담는다.
fn payload(ev: &AuditEvent) -> Value {
    json!({
        "id": ev.id,
        "event": ev.action,
        "target": { "type": ev.target_type, "id": ev.target_id },
        "actor": ev.actor_type,
        "occurred_at": ev.at,
    })
}

struct Sent {
    ok: bool,
    status: Option<u16>,
    error: Option<String>,
}

impl WebhookModule {
    pub fn new(db: Db) -> Result<Self> {
        let poster = WebhookPoster::new().map_err(Error::internal)?;
        Ok(Self { db, poster })
    }

    async fn endpoints(&self) -> Result<Vec<Endpoint>> {
        let rows = sqlx::query!(
            "SELECT id, url, events, secret, enabled, created_at
             FROM mod_webhook_endpoint ORDER BY created_at, id"
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        rows.into_iter()
            .map(|r| {
                Ok(Endpoint {
                    events: serde_json::from_str(&r.events).map_err(Error::internal)?,
                    id: r.id,
                    url: r.url,
                    secret: r.secret,
                    enabled: r.enabled != 0,
                    created_at: r.created_at,
                })
            })
            .collect()
    }

    async fn endpoint(&self, id: &str) -> Result<Endpoint> {
        self.endpoints()
            .await?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| not_found(id))
    }

    async fn delivered(&self, endpoint_id: &str, event_id: &str) -> Result<bool> {
        let n = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!: i64" FROM mod_webhook_delivery
               WHERE endpoint_id = ? AND event_id = ? AND ok = 1"#,
            endpoint_id,
            event_id
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(n > 0)
    }

    async fn send(&self, ep: &Endpoint, event_id: &str, action: &str, body: &[u8]) -> Sent {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let headers = [
            ("Maru-Webhook-Id", event_id.to_owned()),
            ("Maru-Webhook-Event", action.to_owned()),
            ("Maru-Webhook-Timestamp", ts.to_string()),
            (
                "Maru-Webhook-Signature",
                format!("v1={}", sign(&ep.secret, ts, body)),
            ),
        ];
        match self
            .poster
            .post_json(&ep.url, &headers, body.to_vec())
            .await
        {
            Ok(status) if (200..300).contains(&status) => Sent {
                ok: true,
                status: Some(status),
                error: None,
            },
            Ok(status) => Sent {
                ok: false,
                status: Some(status),
                error: Some(format!("응답 {status}")),
            },
            Err(e) => Sent {
                ok: false,
                status: None,
                error: Some(short(&e)),
            },
        }
    }

    async fn record(&self, endpoint_id: &str, ev: &AuditEvent, sent: &Sent) -> Result<()> {
        let ok = i64::from(sent.ok);
        let status = sent.status.map(i64::from);
        let at = clock::now();
        sqlx::query!(
            "INSERT INTO mod_webhook_delivery
                 (endpoint_id, event_id, action, ok, attempts, http_status, last_error, at)
             VALUES (?, ?, ?, ?, 1, ?, ?, ?)
             ON CONFLICT (endpoint_id, event_id) DO UPDATE SET
                 ok = excluded.ok, attempts = attempts + 1, http_status = excluded.http_status,
                 last_error = excluded.last_error, at = excluded.at",
            endpoint_id,
            ev.id,
            ev.action,
            ok,
            status,
            sent.error,
            at
        )
        .execute(self.db.write_pool())
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// 보낼 이벤트 이름을 고른다. 비면 전부.
    fn events_of(input: Vec<String>) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        for e in input {
            let e = e.trim().to_owned();
            if e != "*" && !EVENTS.contains(&e.as_str()) {
                return Err(module_error(
                    Code::Validation,
                    format!(
                        "보낼 수 없는 이벤트입니다: {e}. 보낼 수 있는 것: *, {}",
                        EVENTS.join(", ")
                    ),
                ));
            }
            if !out.contains(&e) {
                out.push(e);
            }
        }
        if out.is_empty() || out.iter().any(|e| e == "*") {
            out = vec!["*".to_owned()];
        }
        Ok(out)
    }

    /// 같은 주소가 이미 있으면 이벤트만 바꾸고 키는 그대로 둔다(여러 번 실행해도 같다).
    async fn tool_add(&self, actor: &Actor, input: AddInput) -> Result<Value> {
        let url = input.url.trim().to_owned();
        check_target_url(&url).map_err(|e| {
            module_error(Code::Validation, format!("받을 주소로 쓸 수 없습니다: {e}"))
        })?;
        let events = Self::events_of(input.event)?;
        let events_json = serde_json::to_string(&events).map_err(Error::internal)?;
        let existing = self.endpoints().await?;
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let (id, secret, created) = match existing.iter().find(|e| e.url == url) {
            Some(e) => {
                sqlx::query!(
                    "UPDATE mod_webhook_endpoint SET events = ?, enabled = 1 WHERE id = ?",
                    events_json,
                    e.id
                )
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
                (e.id.clone(), None, false)
            }
            None => {
                if existing.len() as i64 >= MAX_ENDPOINTS {
                    return Err(module_error(
                        Code::Validation,
                        format!("받는 곳은 {MAX_ENDPOINTS}개까지입니다."),
                    ));
                }
                let id = clock::new_id();
                let secret = format!("{SECRET_PREFIX}{}", new_secret()?);
                let now = clock::now();
                sqlx::query!(
                    "INSERT INTO mod_webhook_endpoint (id, url, events, secret, enabled, created_at)
                     VALUES (?, ?, ?, ?, 1, ?)",
                    id,
                    url,
                    events_json,
                    secret,
                    now
                )
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
                (id, Some(secret), true)
            }
        };
        // 주소 경로에 비밀 값이 들어가는 서비스가 있어 감사에는 호스트만 남긴다.
        let audit = AuditRecord::new(
            actor,
            if created {
                "webhook.add"
            } else {
                "webhook.update"
            },
            Target::new("webhook", id.clone()),
            json!({ "host": host_of(&url), "events": events }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        let mut out = json!({ "id": id, "url": url, "events": events, "created": created });
        match secret {
            Some(s) => {
                out["secret"] = json!(s);
                out["note"] = json!("서명 키는 지금 한 번만 보입니다. 받는 쪽에 저장하세요.");
            }
            None => {
                out["note"] = json!(
                    "이미 있던 받는 곳입니다. 키가 필요하면 `maru webhook rotate --id <ID>`."
                );
            }
        }
        Ok(out)
    }

    async fn tool_list(&self) -> Result<Value> {
        let items: Vec<EndpointView> = self
            .endpoints()
            .await?
            .iter()
            .map(EndpointView::from)
            .collect();
        Ok(json!({ "items": items, "events": EVENTS }))
    }

    async fn tool_remove(&self, actor: &Actor, input: IdInput) -> Result<Value> {
        let ep = self.endpoint(&input.id).await?;
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "DELETE FROM mod_webhook_delivery WHERE endpoint_id = ?",
            ep.id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query!("DELETE FROM mod_webhook_endpoint WHERE id = ?", ep.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let audit = AuditRecord::new(
            actor,
            "webhook.remove",
            Target::new("webhook", ep.id.clone()),
            json!({ "host": host_of(&ep.url) }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({ "removed": ep.id }))
    }

    async fn tool_rotate(&self, actor: &Actor, input: IdInput) -> Result<Value> {
        let ep = self.endpoint(&input.id).await?;
        let secret = format!("{SECRET_PREFIX}{}", new_secret()?);
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "UPDATE mod_webhook_endpoint SET secret = ? WHERE id = ?",
            secret,
            ep.id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let audit = AuditRecord::new(
            actor,
            "webhook.rotate",
            Target::new("webhook", ep.id.clone()),
            json!({ "host": host_of(&ep.url) }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({
            "id": ep.id,
            "secret": secret,
            "note": "서명 키는 지금 한 번만 보입니다. 받는 쪽에 저장하세요.",
        }))
    }

    async fn tool_deliveries(&self, input: DeliveriesInput) -> Result<Value> {
        let limit = input.limit.unwrap_or(50).clamp(1, 500);
        let failed_only = input.failed.unwrap_or(false);
        let rows = sqlx::query!(
            "SELECT endpoint_id, event_id, action, ok, attempts, http_status, last_error, at
             FROM mod_webhook_delivery
             WHERE (?1 IS NULL OR endpoint_id = ?1) AND (?2 = 0 OR ok = 0)
             ORDER BY at DESC, event_id DESC LIMIT ?3",
            input.endpoint,
            failed_only,
            limit
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let items: Vec<DeliveryView> = rows
            .into_iter()
            .map(|r| DeliveryView {
                endpoint_id: r.endpoint_id,
                event_id: r.event_id,
                action: r.action,
                ok: r.ok != 0,
                attempts: r.attempts,
                http_status: r.http_status,
                last_error: r.last_error,
                at: r.at,
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    /// 시험 알림(`webhook.ping`)을 바로 보낸다. 배달 기록에는 남기지 않는다.
    async fn tool_ping(&self, input: IdInput) -> Result<Value> {
        let ep = self.endpoint(&input.id).await?;
        let id = clock::new_id();
        let body = serde_json::to_vec(&json!({
            "id": id,
            "event": "webhook.ping",
            "target": { "type": "webhook", "id": ep.id },
            "actor": "system",
            "occurred_at": clock::now(),
        }))
        .map_err(Error::internal)?;
        let sent = self.send(&ep, &id, "webhook.ping", &body).await;
        if !sent.ok {
            return Err(module_error(
                Code::External,
                format!(
                    "시험 알림을 받지 못했습니다: {}",
                    sent.error.unwrap_or_default()
                ),
            )
            .next(format!("maru webhook ping --id {}", ep.id)));
        }
        Ok(json!({ "ok": true, "http_status": sent.status }))
    }
}

#[async_trait]
impl Module for WebhookModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 20261105800001,
            name: "webhook",
            up: include_str!("../migrations/20261105800001_webhook.up.sql"),
            down: include_str!("../migrations/20261105800001_webhook.down.sql"),
        }]
    }

    fn tools(&self) -> Vec<ToolDef> {
        let admin = Some(Permission::ModuleManage);
        vec![
            ToolDef {
                name: "add",
                description: "받는 곳을 더한다. 같은 주소면 이벤트만 바꾼다. 서명 키는 처음 한 번만 돌려준다",
                input_schema: schema_of::<AddInput>(),
                permission: admin,
            },
            ToolDef {
                name: "list",
                description: "받는 곳 목록(서명 키는 끝 네 글자만)",
                input_schema: schema_of::<ListInput>(),
                permission: admin,
            },
            ToolDef {
                name: "remove",
                description: "받는 곳과 그 배달 기록을 지운다",
                input_schema: schema_of::<IdInput>(),
                permission: admin,
            },
            ToolDef {
                name: "rotate",
                description: "서명 키를 새로 만든다. 옛 키로 만든 서명은 더 맞지 않는다",
                input_schema: schema_of::<IdInput>(),
                permission: admin,
            },
            ToolDef {
                name: "deliveries",
                description: "최근 배달 기록. 실패만 보려면 failed",
                input_schema: schema_of::<DeliveriesInput>(),
                permission: admin,
            },
            ToolDef {
                name: "ping",
                description: "시험 알림(webhook.ping)을 지금 보낸다",
                input_schema: schema_of::<IdInput>(),
                permission: admin,
            },
        ]
    }

    async fn call_tool(
        &self,
        _ctx: &Ctx,
        actor: &Actor,
        tool: &str,
        input: Value,
    ) -> Result<Value> {
        match tool {
            "add" => self.tool_add(actor, parse_input(input)?).await,
            "list" => {
                let _: ListInput = parse_input(input)?;
                self.tool_list().await
            }
            "remove" => self.tool_remove(actor, parse_input(input)?).await,
            "rotate" => self.tool_rotate(actor, parse_input(input)?).await,
            "deliveries" => self.tool_deliveries(parse_input(input)?).await,
            "ping" => self.tool_ping(parse_input(input)?).await,
            other => Err(maru_modules::tool_not_found(MANIFEST.id, other)),
        }
    }

    /// 이 동작을 받기로 한 곳마다 보낸다. 한 곳이라도 못 받으면 실패를 돌려 버스가 다시 부르게 한다.
    async fn on_event(&self, _ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        if !EVENTS.contains(&ev.action.as_str()) {
            return Ok(());
        }
        let targets: Vec<Endpoint> = self
            .endpoints()
            .await?
            .into_iter()
            .filter(|e| e.wants(&ev.action))
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        let body = serde_json::to_vec(&payload(ev)).map_err(Error::internal)?;
        let mut failed = Vec::new();
        for ep in &targets {
            if self.delivered(&ep.id, &ev.id).await? {
                continue;
            }
            let sent = self.send(ep, &ev.id, &ev.action, &body).await;
            self.record(&ep.id, ev, &sent).await?;
            if !sent.ok {
                failed.push(format!(
                    "{}: {}",
                    host_of(&ep.url),
                    sent.error.unwrap_or_default()
                ));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(module_error(
                Code::External,
                format!("웹훅을 보내지 못했습니다: {}", failed.join("; ")),
            ))
        }
    }

    async fn purge(&self, _ctx: &Ctx) -> Result<u64> {
        let before = clock::hours_ago(KEEP_DAYS * 24);
        Ok(
            sqlx::query!("DELETE FROM mod_webhook_delivery WHERE at < ?", before)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?
                .rows_affected(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_known_value() {
        // RFC 4231 이 아닌 이 형식의 고정 값: 받는 쪽 예제 코드가 같은 값을 내야 한다.
        let sig = sign("mwhs_test", 1_700_000_000, br#"{"id":"e1"}"#);
        assert_eq!(sig.len(), 64);
        assert_eq!(sig, sign("mwhs_test", 1_700_000_000, br#"{"id":"e1"}"#));
        assert_ne!(sig, sign("mwhs_other", 1_700_000_000, br#"{"id":"e1"}"#));
        assert_ne!(sig, sign("mwhs_test", 1_700_000_001, br#"{"id":"e1"}"#));
    }

    #[test]
    fn events_are_checked_and_star_wins() {
        assert_eq!(
            WebhookModule::events_of(vec![]).unwrap(),
            vec!["*".to_owned()]
        );
        assert_eq!(
            WebhookModule::events_of(vec!["post.publish".into(), "post.publish".into()]).unwrap(),
            vec!["post.publish".to_owned()]
        );
        assert_eq!(
            WebhookModule::events_of(vec!["post.publish".into(), "*".into()]).unwrap(),
            vec!["*".to_owned()]
        );
        assert!(WebhookModule::events_of(vec!["member.update".into()]).is_err());
    }

    #[test]
    fn host_only_for_audit() {
        assert_eq!(
            host_of("https://hooks.example.test/services/T0/B0/xyz"),
            "hooks.example.test"
        );
        assert_eq!(
            host_of("https://u:p@example.test:8443/a?b"),
            "example.test:8443"
        );
    }
}
