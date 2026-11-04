//! 알림 모듈 (GAP M11, ADR-0069). 내 글의 댓글, 내 댓글의 답글, 내 글의 답변글을 사이트 안과 메일로 알린다.
//!
//! - 원천은 감사 기록이고 배달은 최소 한 번이다(ADR-0066). `(member_id, source)` 가 유일해서 다시 받아도 한 건이다.
//! - 알림과 편지는 같은 트랜잭션에 쌓는다. 재시도·실패 보관은 모듈 배달함과 보낼 편지함이 맡는다.
//! - 본문은 복사하지 않는다. 글·댓글을 지우거나 가리면 알림에서도 볼 수 없어야 하기 때문이다.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use maru_core::actor::{Actor, Permission};
use maru_core::audit::{AuditRecord, Target};
use maru_core::fields::FieldDef;
use maru_core::model::{AuditEvent, CommentStatus, MailOut, MailStatus, MemberStatus};
use maru_core::model::{Member, Post, PostStatus};
use maru_core::services::{auth, mail, posts, settings};
use maru_core::store::ModuleMigration;
use maru_core::urls::{self, Target as UrlTarget};
use maru_core::{Code, Ctx, Error, Result, clock};
use maru_modules::service::settings_of;
use maru_modules::{
    ME_SECTIONS, Manifest, Module, ModuleState, SlotContent, SlotView, ToolDef, module_error,
    schema_of,
};
use maru_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

static MANIFEST: Manifest = Manifest {
    id: "notify",
    name: "알림",
    version: env!("CARGO_PKG_VERSION"),
    description: "내 글의 댓글, 내 댓글의 답글, 내 글의 답변글을 사이트 안과 메일로 알립니다.",
    events: &[
        "comment.create",
        "comment.approve",
        "comment.hide",
        "comment.spam",
        "comment.delete",
        "post.write",
        "post.publish",
        "post.publish_scheduled",
        "post.delete",
        "member.withdraw",
    ],
};

const PER_PAGE: i64 = 20;
const ME_LIMIT: i64 = 5;
const HOME: &str = "/m/notify";
/// 알림 보관 날수 (ADR-0073)
const DEFAULT_KEEP_DAYS: i64 = 180;
const MIN_KEEP_DAYS: i64 = 7;
const MAX_KEEP_DAYS: i64 = 3650;

/// 알림 종류. 표에는 문자열로 둔다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Comment,
    Reply,
    Answer,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Reply => "reply",
            Self::Answer => "answer",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "comment" => Some(Self::Comment),
            "reply" => Some(Self::Reply),
            "answer" => Some(Self::Answer),
            _ => None,
        }
    }

    /// 화면에 보이는 한 줄. 받침을 피하려고 이름 뒤에 「님」을 붙인다.
    fn sentence(self, who: &str) -> String {
        match self {
            Self::Comment => format!("{who}님이 내 글에 댓글을 남겼습니다."),
            Self::Reply => format!("{who}님이 내 댓글에 답글을 남겼습니다."),
            Self::Answer => format!("{who}님이 내 글에 답변글을 남겼습니다."),
        }
    }

    fn subject(self) -> &'static str {
        match self {
            Self::Comment => "내 글에 새 댓글이 달렸습니다",
            Self::Reply => "내 댓글에 답글이 달렸습니다",
            Self::Answer => "내 글에 답변글이 달렸습니다",
        }
    }
}

/// 한 사람에게 보낼 알림 하나.
struct Note<'a> {
    member_id: &'a str,
    kind: Kind,
    /// `comment:<id>` 또는 `post:<id>`. 다시 받아도 한 건으로 묶는 키
    source: String,
    post_id: &'a str,
    comment_id: Option<&'a str>,
    who: &'a str,
}

#[derive(Clone)]
pub struct NotifyModule {
    db: Db,
}

/// 알림 한 줄. 볼 수 없게 된 글이면 주소와 제목을 비운다.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NotifyRow {
    pub id: String,
    pub kind: String,
    pub post_id: String,
    pub text: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub unread: bool,
    pub date: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// 회원 ID. 비우면 나. 다른 회원은 회원 관리 권한이 있어야 한다
    #[serde(default)]
    member: Option<String>,
    /// 읽지 않은 것만
    #[serde(default)]
    unread: bool,
    #[serde(default)]
    page: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UnreadInput {
    /// 회원 ID. 비우면 나
    #[serde(default)]
    member: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    /// 읽음으로 둘 알림 ID. 비우면 내 알림 전부
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailInput {
    /// 메일로도 받을지
    mail: bool,
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn self_only() -> Error {
    module_error(Code::Forbidden, "알림은 회원 본인만 바꿀 수 있습니다.")
}

/// 편지에 넣기 전에 줄바꿈 같은 제어 문자를 뺀다. 이름과 제목은 사용자가 쓴 값이다.
fn plain(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

fn audit_of(
    actor: &Actor,
    action: &'static str,
    kind: &'static str,
    id: &str,
    diff: Value,
) -> AuditRecord {
    AuditRecord::new(actor, action, Target::new(kind, id), diff)
}

impl NotifyModule {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 이 사람이 지금 볼 수 있는 글의 주소와 제목. 볼 수 없으면 `None`.
    async fn visible(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        post_id: &str,
    ) -> Result<Option<(String, String)>> {
        let Some(post) = ctx.store.post_by_id(post_id).await? else {
            return Ok(None);
        };
        let Some(board) = ctx.store.board_by_id(&post.board_id).await? else {
            return Ok(None);
        };
        match posts::public_post(ctx, actor, &board.slug, &post.slug).await {
            Ok((board, post)) => {
                let url = urls::path(&UrlTarget::Post {
                    board: &board.slug,
                    post: &post.slug,
                });
                Ok(Some((url, post.title)))
            }
            Err(e) if matches!(e.code(), Code::NotFound | Code::Forbidden) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn name_of(
        &self,
        ctx: &Ctx,
        author: Option<&str>,
        guest: Option<&str>,
    ) -> Result<String> {
        if let Some(id) = author {
            return Ok(ctx
                .store
                .member_by_id(id)
                .await?
                .map(|m| m.display_name)
                .unwrap_or_else(|| "탈퇴한 회원".into()));
        }
        Ok(guest.unwrap_or("운영자").to_owned())
    }

    /// 사이트 설정과 본인 설정이 모두 켜져 있고, 주소를 확인한 회원에게만 편지를 보낸다.
    async fn wants_mail(&self, ctx: &Ctx, member: &Member) -> Result<bool> {
        if member.email_verified_at.is_none() {
            return Ok(false);
        }
        let site_on = settings_of(ctx, self)
            .await?
            .get("mail")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok(site_on && self.mail_pref(&member.id).await?)
    }

    async fn mail_pref(&self, member_id: &str) -> Result<bool> {
        let row = sqlx::query_scalar!(
            "SELECT mail FROM mod_notify_pref WHERE member_id = ?",
            member_id
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(row.is_none_or(|m| m != 0))
    }

    /// 받는 사람이 그 글을 볼 수 있을 때만 쌓는다. 같은 `source` 는 한 번뿐이다.
    async fn push(&self, ctx: &Ctx, note: Note<'_>) -> Result<()> {
        let Some(member) = ctx.store.member_by_id(note.member_id).await? else {
            return Ok(());
        };
        if member.status != MemberStatus::Active {
            return Ok(());
        }
        let actor = auth::member_actor(ctx, &member).await?;
        let Some((url, title)) = self.visible(ctx, &actor, note.post_id).await? else {
            return Ok(());
        };
        let wants_mail = self.wants_mail(ctx, &member).await?;
        let site_name = settings::site(ctx).await?.name;

        let id = clock::new_id();
        let at = clock::now();
        let kind = note.kind.as_str();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let added = sqlx::query!(
            "INSERT OR IGNORE INTO mod_notify_item
                 (id, member_id, kind, source, post_id, comment_id, at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            id,
            note.member_id,
            kind,
            note.source,
            note.post_id,
            note.comment_id,
            at
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected()
            > 0;
        if added && wants_mail {
            // 인기 글에 편지가 쏟아지지 않게 10분 칸마다 글·종류별로 한 통만 보낸다.
            let idem_key = format!(
                "notify:{}:{}:{kind}:{}",
                member.id,
                note.post_id,
                mail::window()
            );
            let base = ctx.site_url.trim_end_matches('/');
            let link = match note.comment_id {
                Some(c) => format!("{base}{url}#comment-{c}"),
                None => format!("{base}{url}"),
            };
            let body = format!(
                "{}\n\n글: {}\n보기: {link}\n\n모든 알림과 메일 받기 설정: {base}{HOME}\n",
                note.kind.sentence(&plain(note.who)),
                plain(&title),
            );
            let out = MailOut {
                id: clock::new_id(),
                idem_key,
                to_addr: member.email.clone(),
                subject: format!("[{site_name}] {}", note.kind.subject()),
                body,
                status: MailStatus::Queued,
                attempts: 0,
                last_error: None,
                created_at: at.clone(),
                sent_at: None,
            };
            maru_storage::mail::queue(&mut tx, &out).await?;
            let audit = audit_of(
                &Actor::local(),
                "notify.mail",
                "member",
                &member.id,
                json!({ "kind": kind, "source": note.source }),
            );
            maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// 댓글이 공개되면: 답한 댓글의 작성자에게 답글 알림, 글 작성자에게 댓글 알림. 자기 자신은 빼고, 한 사람에게 하나만.
    async fn on_comment(&self, ctx: &Ctx, id: &str) -> Result<()> {
        let Some(comment) = ctx.store.comment_by_id(id).await? else {
            return Ok(());
        };
        if comment.status != CommentStatus::Published {
            return Ok(());
        }
        let Some(post) = ctx.store.post_by_id(&comment.post_id).await? else {
            return Ok(());
        };
        let who = self
            .name_of(
                ctx,
                comment.author_id.as_deref(),
                comment.guest_name.as_deref(),
            )
            .await?;
        let mut replied: Option<String> = None;
        if let Some(target) = comment.reply_to_id.as_ref().or(comment.parent_id.as_ref())
            && let Some(parent) = ctx.store.comment_by_id(target).await?
            && parent.status == CommentStatus::Published
        {
            replied = parent.author_id;
        }
        let source = format!("comment:{id}");
        let note = |member_id, kind| Note {
            member_id,
            kind,
            source: source.clone(),
            post_id: &post.id,
            comment_id: Some(id),
            who: &who,
        };
        if let Some(m) = &replied
            && comment.author_id.as_ref() != Some(m)
        {
            self.push(ctx, note(m, Kind::Reply)).await?;
        }
        if let Some(owner) = &post.author_id
            && comment.author_id.as_ref() != Some(owner)
            && replied.as_ref() != Some(owner)
        {
            self.push(ctx, note(owner, Kind::Comment)).await?;
        }
        Ok(())
    }

    /// 답변글이 공개되면 바로 위 글의 작성자에게 알린다.
    async fn on_post(&self, ctx: &Ctx, id: &str) -> Result<()> {
        let Some(answer) = ctx.store.post_by_id(id).await? else {
            return Ok(());
        };
        if answer.status != PostStatus::Published {
            return Ok(());
        }
        let Some(parent_id) = &answer.parent_id else {
            return Ok(());
        };
        let Some(Post {
            author_id: Some(owner),
            ..
        }) = ctx.store.post_by_id(parent_id).await?
        else {
            return Ok(());
        };
        if answer.author_id.as_ref() == Some(&owner) {
            return Ok(());
        }
        let who = self
            .name_of(
                ctx,
                answer.author_id.as_deref(),
                answer.guest_name.as_deref(),
            )
            .await?;
        self.push(
            ctx,
            Note {
                member_id: &owner,
                kind: Kind::Answer,
                source: format!("post:{id}"),
                post_id: id,
                comment_id: None,
                who: &who,
            },
        )
        .await
    }

    /// 한 줄을 지금 시점으로 푼다. 지워진 원본은 이름 없이 보여 준다.
    async fn row(&self, ctx: &Ctx, actor: &Actor, item: RawItem, tz: i32) -> Result<NotifyRow> {
        let kind = Kind::parse(&item.kind).unwrap_or(Kind::Comment);
        let visible = self.visible(ctx, actor, &item.post_id).await?;
        let who = match &item.comment_id {
            Some(c) => match ctx.store.comment_by_id(c).await? {
                Some(c) => Some(
                    self.name_of(ctx, c.author_id.as_deref(), c.guest_name.as_deref())
                        .await?,
                ),
                None => None,
            },
            None => match ctx.store.post_by_id(&item.post_id).await? {
                Some(p) => Some(
                    self.name_of(ctx, p.author_id.as_deref(), p.guest_name.as_deref())
                        .await?,
                ),
                None => None,
            },
        };
        let text = match (&visible, who) {
            (Some(_), Some(who)) => kind.sentence(&who),
            _ => "볼 수 없게 된 글의 알림입니다.".to_owned(),
        };
        let (url, title) = match visible {
            Some((url, title)) => {
                let url = match &item.comment_id {
                    Some(c) => format!("{url}#comment-{c}"),
                    None => url,
                };
                (Some(url), Some(title))
            }
            None => (None, None),
        };
        Ok(NotifyRow {
            id: item.id,
            kind: item.kind,
            post_id: item.post_id,
            text,
            title,
            url,
            unread: item.read_at.is_none(),
            date: clock::display_date(&item.at, tz),
        })
    }

    /// 새것부터. (줄들, 조건에 맞는 전체 수)
    async fn rows(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        member_id: &str,
        unread_only: bool,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<NotifyRow>, i64)> {
        let only = i64::from(unread_only);
        let items = sqlx::query_as!(
            RawItem,
            "SELECT id, kind, post_id, comment_id, at, read_at FROM mod_notify_item
             WHERE member_id = ? AND (? = 0 OR read_at IS NULL)
             ORDER BY at DESC, id DESC LIMIT ? OFFSET ?",
            member_id,
            only,
            limit,
            offset
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let total = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!: i64" FROM mod_notify_item
               WHERE member_id = ? AND (? = 0 OR read_at IS NULL)"#,
            member_id,
            only
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let tz = settings::site(ctx).await?.tz_offset_min;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(self.row(ctx, actor, item, tz).await?);
        }
        Ok((out, total))
    }

    async fn unread(&self, member_id: &str) -> Result<i64> {
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!: i64" FROM mod_notify_item
               WHERE member_id = ? AND read_at IS NULL"#,
            member_id
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)
    }

    /// 내 알림 하나를 읽음으로 둔다. 남의 알림이면 없는 것과 같다.
    async fn mark(&self, actor: &Actor, member_id: &str, id: &str) -> Result<RawItem> {
        let now = clock::now();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let item = sqlx::query_as!(
            RawItem,
            "SELECT id, kind, post_id, comment_id, at, read_at FROM mod_notify_item
             WHERE id = ? AND member_id = ?",
            id,
            member_id
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| module_error(Code::NotFound, "알림을 찾을 수 없습니다."))?;
        if item.read_at.is_none() {
            sqlx::query!(
                "UPDATE mod_notify_item SET read_at = ? WHERE id = ?",
                now,
                id
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let audit = audit_of(actor, "notify.read", "notify_item", id, json!({}));
            maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(item)
    }

    async fn mark_all(&self, actor: &Actor, member_id: &str) -> Result<u64> {
        let now = clock::now();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let n = sqlx::query!(
            "UPDATE mod_notify_item SET read_at = ? WHERE member_id = ? AND read_at IS NULL",
            now,
            member_id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n > 0 {
            let audit = audit_of(
                actor,
                "notify.read_all",
                "member",
                member_id,
                json!({ "count": n }),
            );
            maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(n)
    }

    async fn set_mail(&self, actor: &Actor, member_id: &str, on: bool) -> Result<()> {
        let value = i64::from(on);
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "INSERT INTO mod_notify_pref (member_id, mail) VALUES (?, ?)
             ON CONFLICT (member_id) DO UPDATE SET mail = excluded.mail",
            member_id,
            value
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let audit = audit_of(
            actor,
            "notify.mail_pref",
            "member",
            member_id,
            json!({ "mail": on }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)
    }

    /// 도구의 대상 회원. 비우면 나, 남이면 회원 관리 권한이 있어야 한다.
    fn target(&self, actor: &Actor, member: Option<String>) -> Result<String> {
        match member {
            Some(m) if actor.member_id() != Some(&m) => {
                actor.require(Permission::MemberManage)?;
                Ok(m)
            }
            Some(m) => Ok(m),
            None => actor.member_id().cloned().ok_or_else(|| {
                module_error(Code::Validation, "회원을 적으세요: --member <회원 ID>")
            }),
        }
    }

    async fn tool_list(&self, ctx: &Ctx, actor: &Actor, input: ListInput) -> Result<Value> {
        let member = self.target(actor, input.member)?;
        let page = input.page.unwrap_or(1).max(1);
        let (items, total) = self
            .rows(
                ctx,
                actor,
                &member,
                input.unread,
                PER_PAGE,
                (page - 1) * PER_PAGE,
            )
            .await?;
        Ok(json!({
            "items": items,
            "total": total,
            "page": page,
            "unread": self.unread(&member).await?,
        }))
    }

    async fn tool_read(&self, actor: &Actor, input: ReadInput) -> Result<Value> {
        let member = actor.member_id().ok_or_else(self_only)?;
        match input.id {
            Some(id) => {
                self.mark(actor, member, &id).await?;
                Ok(json!({ "read": 1 }))
            }
            None => Ok(json!({ "read": self.mark_all(actor, member).await? })),
        }
    }
}

/// 표의 한 줄.
#[derive(Debug)]
struct RawItem {
    id: String,
    kind: String,
    post_id: String,
    comment_id: Option<String>,
    at: String,
    read_at: Option<String>,
}

#[async_trait]
impl Module for NotifyModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 20261105400001,
            name: "notify",
            up: include_str!("../migrations/20261105400001_notify.up.sql"),
            down: include_str!("../migrations/20261105400001_notify.down.sql"),
        }]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/", get(list_page))
                .route("/open/{id}", post(open_form))
                .route("/read-all", post(read_all_form))
                .route("/prefs", post(prefs_form))
                .layer(Extension(Arc::new(self.clone()))),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("mod/notify/me.html", include_str!("../templates/me.html")),
            ("mod/notify/row.html", include_str!("../templates/row.html")),
            (
                "mod/notify/list.html",
                include_str!("../templates/list.html"),
            ),
        ]
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "list",
                description: "회원의 알림 목록",
                input_schema: schema_of::<ListInput>(),
                permission: None,
            },
            ToolDef {
                name: "unread",
                description: "읽지 않은 알림 수",
                input_schema: schema_of::<UnreadInput>(),
                permission: None,
            },
            ToolDef {
                name: "read",
                description: "회원 본인의 알림을 읽음으로 둔다. ID 를 비우면 전부",
                input_schema: schema_of::<ReadInput>(),
                permission: None,
            },
            ToolDef {
                name: "mail",
                description: "회원 본인의 메일 알림을 켜거나 끈다",
                input_schema: schema_of::<MailInput>(),
                permission: None,
            },
        ]
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        match tool {
            "list" => self.tool_list(ctx, actor, parse_input(input)?).await,
            "unread" => {
                let input: UnreadInput = parse_input(input)?;
                let member = self.target(actor, input.member)?;
                Ok(json!({ "unread": self.unread(&member).await? }))
            }
            "read" => self.tool_read(actor, parse_input(input)?).await,
            "mail" => {
                let input: MailInput = parse_input(input)?;
                let member = actor.member_id().ok_or_else(self_only)?;
                self.set_mail(actor, member, input.mail).await?;
                Ok(json!({ "mail": input.mail }))
            }
            other => Err(maru_modules::tool_not_found(MANIFEST.id, other)),
        }
    }

    async fn on_event(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        let Some(id) = &ev.target_id else {
            return Ok(());
        };
        match ev.action.as_str() {
            "comment.create" | "comment.approve" => self.on_comment(ctx, id).await,
            "post.write" | "post.publish" | "post.publish_scheduled" => self.on_post(ctx, id).await,
            // 가린 댓글의 알림은 거둔다. 다시 승인하면 위에서 다시 쌓인다(편지는 한 번뿐).
            "comment.hide" | "comment.spam" | "comment.delete" => {
                let source = format!("comment:{id}");
                sqlx::query!("DELETE FROM mod_notify_item WHERE source = ?", source)
                    .execute(self.db.write_pool())
                    .await
                    .map_err(Error::internal)?;
                Ok(())
            }
            "post.delete" => {
                sqlx::query!("DELETE FROM mod_notify_item WHERE post_id = ?", id)
                    .execute(self.db.write_pool())
                    .await
                    .map_err(Error::internal)?;
                Ok(())
            }
            "member.withdraw" => {
                sqlx::query!("DELETE FROM mod_notify_item WHERE member_id = ?", id)
                    .execute(self.db.write_pool())
                    .await
                    .map_err(Error::internal)?;
                sqlx::query!("DELETE FROM mod_notify_pref WHERE member_id = ?", id)
                    .execute(self.db.write_pool())
                    .await
                    .map_err(Error::internal)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `keep_days` 가 지난 알림을 지운다 (읽음과 상관없이).
    async fn purge(&self, ctx: &Ctx) -> Result<u64> {
        let days = settings_of(ctx, self)
            .await?
            .get("keep_days")
            .and_then(Value::as_i64)
            .unwrap_or(DEFAULT_KEEP_DAYS)
            .clamp(MIN_KEEP_DAYS, MAX_KEEP_DAYS);
        let before = clock::hours_ago(days * 24);
        Ok(
            sqlx::query!("DELETE FROM mod_notify_item WHERE at < ?", before)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?
                .rows_affected(),
        )
    }

    fn settings_fields(&self) -> Vec<FieldDef> {
        vec![
            FieldDef {
                key: "mail".to_owned(),
                label: "메일로도 알리기".to_owned(),
                kind: "boolean".to_owned(),
                required: false,
                options: Vec::new(),
                help: Some(
                    "끄면 사이트 안에서만 알립니다. 회원은 알림 화면에서 따로 끌 수 있습니다."
                        .to_owned(),
                ),
            },
            FieldDef {
                key: "keep_days".to_owned(),
                label: "알림 보관 날수".to_owned(),
                kind: "number".to_owned(),
                required: false,
                options: Vec::new(),
                help: Some(format!(
                    "이 날수가 지난 알림은 하루 한 번 지웁니다 ({MIN_KEEP_DAYS}~{MAX_KEEP_DAYS})."
                )),
            },
        ]
    }

    fn default_settings(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("mail".into(), json!(true));
        m.insert("keep_days".into(), json!(DEFAULT_KEEP_DAYS));
        m
    }

    fn check_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        if !settings
            .get("keep_days")
            .and_then(Value::as_i64)
            .is_some_and(|n| (MIN_KEEP_DAYS..=MAX_KEEP_DAYS).contains(&n))
        {
            return Err(module_error(
                Code::Validation,
                format!("keep_days 는 {MIN_KEEP_DAYS}~{MAX_KEEP_DAYS} 사이의 정수여야 합니다."),
            ));
        }
        Ok(())
    }

    async fn slot(
        &self,
        ctx: &Ctx,
        slot: &str,
        view: &SlotView<'_>,
    ) -> Result<Option<SlotContent>> {
        if slot != ME_SECTIONS {
            return Ok(None);
        }
        let Some(member) = view.viewer.member_id() else {
            return Ok(None);
        };
        let (items, _) = self
            .rows(ctx, view.viewer, member, false, ME_LIMIT, 0)
            .await?;
        Ok(Some(SlotContent {
            template: "mod/notify/me.html",
            data: json!({
                "items": items,
                "unread": self.unread(member).await?,
                "more_url": HOME,
            }),
        }))
    }
}

/// 공개 폼의 앞머리: 같은 사이트에서 왔고 로그인한 회원인가. 아니면 돌려줄 응답.
async fn member_or_response(
    state: &ModuleState,
    headers: &HeaderMap,
) -> std::result::Result<(Actor, String), Box<Response>> {
    if !state.host.same_origin(headers) {
        return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
    }
    let actor = match state.host.viewer(headers).await {
        Ok(a) => a,
        Err(e) => return Err(Box::new(state.host.error(&e).await)),
    };
    match actor.member_id().cloned() {
        Some(member) => Ok((actor, member)),
        None => Err(Box::new(
            Redirect::to(&urls::with_query("/login", &[("next", HOME)])).into_response(),
        )),
    }
}

/// 알림을 읽음으로 두고 원본으로 보낸다. 주소는 폼이 아니라 저장된 값에서 만든다.
async fn open_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<NotifyModule>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let (actor, member) = match member_or_response(&state, &headers).await {
        Ok(v) => v,
        Err(res) => return *res,
    };
    let item = match module.mark(&actor, &member, &id).await {
        Ok(item) => item,
        Err(e) => return state.host.error(&e).await,
    };
    let target = match module.visible(&state.ctx, &actor, &item.post_id).await {
        Ok(Some((url, _))) => match item.comment_id {
            Some(c) => format!("{url}#comment-{c}"),
            None => url,
        },
        Ok(None) => HOME.to_owned(),
        Err(e) => return state.host.error(&e).await,
    };
    Redirect::to(&target).into_response()
}

async fn read_all_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<NotifyModule>>,
    headers: HeaderMap,
) -> Response {
    let (actor, member) = match member_or_response(&state, &headers).await {
        Ok(v) => v,
        Err(res) => return *res,
    };
    match module.mark_all(&actor, &member).await {
        Ok(_) => Redirect::to(HOME).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct PrefsForm {
    /// 체크하면 `on`, 안 하면 오지 않는다.
    #[serde(default)]
    mail: Option<String>,
}

async fn prefs_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<NotifyModule>>,
    headers: HeaderMap,
    Form(f): Form<PrefsForm>,
) -> Response {
    let (actor, member) = match member_or_response(&state, &headers).await {
        Ok(v) => v,
        Err(res) => return *res,
    };
    match module.set_mail(&actor, &member, f.mail.is_some()).await {
        Ok(()) => Redirect::to(HOME).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    page: Option<i64>,
}

async fn list_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<NotifyModule>>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    let Some(member) = actor.member_id() else {
        return Redirect::to(&urls::with_query("/login", &[("next", HOME)])).into_response();
    };
    let page = q.page.unwrap_or(1).max(1);
    let loaded = async {
        let (items, total) = module
            .rows(
                &state.ctx,
                &actor,
                member,
                false,
                PER_PAGE,
                (page - 1) * PER_PAGE,
            )
            .await?;
        let unread = module.unread(member).await?;
        let mail_on = module.mail_pref(member).await?;
        let site_mail = settings_of(&state.ctx, module.as_ref())
            .await?
            .get("mail")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Ok::<_, Error>((items, total, unread, mail_on, site_mail))
    }
    .await;
    match loaded {
        Ok((items, total, unread, mail_on, site_mail)) => {
            state
                .host
                .page(
                    "mod/notify/list.html",
                    "알림",
                    json!({
                        "items": items,
                        "total": total,
                        "unread": unread,
                        "mail_on": mail_on,
                        "site_mail": site_mail,
                        "page": page,
                        "prev": (page > 1).then_some(page - 1),
                        "next": (page * PER_PAGE < total).then_some(page + 1),
                        "noindex": true,
                    }),
                )
                .await
        }
        Err(e) => state.host.error(&e).await,
    }
}
