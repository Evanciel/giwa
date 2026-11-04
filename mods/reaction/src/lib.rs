//! 반응 모듈 (GAP M7, ADR-0068). 글과 댓글에 추천·비추천·이모지를 한 사람 하나씩 남긴다.
//!
//! 같은 것을 다시 누르면 취소, 다른 것을 누르면 바뀐다. 손님 반응은 기본으로 꺼져 있고, 켜면
//! 연결 주소와 브라우저로 만든 해시를 쓴다. 완전한 한 사람 보장은 아니다 (ADR-0068).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{ConnectInfo, Form, Path, State};
use axum::http::{Extensions, HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Extension, Router};
use maru_core::actor::{Actor, Permission};
use maru_core::audit::{AuditRecord, Target};
use maru_core::fields::FieldDef;
use maru_core::model::{AuditEvent, CommentStatus};
use maru_core::services::{auth, posts};
use maru_core::store::ModuleMigration;
use maru_core::urls::{self, Target as UrlTarget};
use maru_core::{Code, Ctx, Error, Result, clock};
use maru_modules::service::settings_of;
use maru_modules::{
    COMMENT_ACTIONS, Manifest, Module, ModuleState, POST_FOOTER, POST_LIST, SlotContent, SlotView,
    ToolDef, module_error, schema_of,
};
use maru_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

static MANIFEST: Manifest = Manifest {
    id: "reaction",
    name: "반응",
    version: env!("CARGO_PKG_VERSION"),
    description: "글과 댓글에 추천·비추천·이모지를 남깁니다.",
    events: &["member.withdraw"],
};

/// 고를 수 있는 반응. 사이트가 설정(`kinds`)으로 이 중에서 고른다.
const KINDS: [(&str, &str); 6] = [
    ("up", "추천"),
    ("down", "비추천"),
    ("heart", "❤️ 좋아요"),
    ("laugh", "😄 웃겨요"),
    ("wow", "😮 놀라워요"),
    ("sad", "😢 슬퍼요"),
];
const DEFAULT_KINDS: &str = "up,down";

#[derive(Clone)]
pub struct ReactionModule {
    db: Db,
}

/// 반응을 남기는 사람. 회원은 회원 ID, 손님은 해시.
struct Who {
    key: String,
    member_id: Option<String>,
}

/// 누른 대상의 글쓴이와 돌아갈 주소.
struct Resolved {
    author: Option<String>,
    url: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct KindCount {
    pub id: String,
    pub label: String,
    pub count: i64,
    pub pressed: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TargetInput {
    /// post 또는 comment
    target_type: String,
    target_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReactInput {
    /// post 또는 comment
    target_type: String,
    target_id: String,
    /// up, down, heart, laugh, wow, sad 중 사이트가 켠 것
    kind: String,
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn gone() -> Error {
    module_error(Code::NotFound, "반응할 글이나 댓글을 찾을 수 없습니다.")
}

fn check_type(target_type: &str) -> Result<()> {
    if matches!(target_type, "post" | "comment") {
        Ok(())
    } else {
        Err(module_error(
            Code::Validation,
            format!("반응 대상은 post 또는 comment 입니다: {target_type}"),
        ))
    }
}

/// 설정의 `kinds` 를 고를 수 있는 순서대로.
fn kinds_of(settings: &Map<String, Value>) -> Vec<(&'static str, &'static str)> {
    let text = settings
        .get("kinds")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_KINDS);
    let wanted: Vec<&str> = text.split(',').map(str::trim).collect();
    KINDS
        .into_iter()
        .filter(|(id, _)| wanted.contains(id))
        .collect()
}

fn flag(settings: &Map<String, Value>, key: &str) -> bool {
    settings.get(key).and_then(Value::as_bool).unwrap_or(false)
}

impl ReactionModule {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 대상이 지금 이 사람에게 보이는지 확인하고 글쓴이와 주소를 찾는다.
    async fn resolve(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        target_type: &str,
        target_id: &str,
    ) -> Result<Resolved> {
        check_type(target_type)?;
        let (post_id, comment) = if target_type == "post" {
            (target_id.to_owned(), None)
        } else {
            let comment = ctx
                .store
                .comment_by_id(target_id)
                .await?
                .filter(|c| c.status == CommentStatus::Published)
                .ok_or_else(gone)?;
            (comment.post_id.clone(), Some(comment))
        };
        let post = ctx.store.post_by_id(&post_id).await?.ok_or_else(gone)?;
        let board = ctx
            .store
            .board_by_id(&post.board_id)
            .await?
            .ok_or_else(gone)?;
        // 비공개·비밀글·읽기 권한은 공개 화면과 같은 검사를 쓴다.
        let (_, post) = posts::public_post(ctx, actor, &board.slug, &post.slug)
            .await
            .map_err(|e| {
                if e.code() == Code::NotFound {
                    gone()
                } else {
                    e
                }
            })?;
        let url = urls::path(&UrlTarget::Post {
            board: &board.slug,
            post: &post.slug,
        });
        Ok(match comment {
            Some(c) => Resolved {
                author: c.author_id,
                url: format!("{url}#comment-{}", c.id),
            },
            None => Resolved {
                author: post.author_id,
                url: format!("{url}#reactions"),
            },
        })
    }

    /// 누르기. 같은 것이면 취소, 다른 것이면 바꾼다. 돌아갈 주소를 준다.
    async fn press(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        who: &Who,
        target_type: &str,
        target_id: &str,
        kind: &str,
    ) -> Result<(Option<String>, String)> {
        let settings = settings_of(ctx, self).await?;
        if !kinds_of(&settings).iter().any(|(id, _)| *id == kind) {
            return Err(module_error(
                Code::Validation,
                format!("이 사이트에서 쓰지 않는 반응입니다: {kind}"),
            ));
        }
        if target_type == "comment" && !flag(&settings, "comments") {
            return Err(module_error(
                Code::Forbidden,
                "이 사이트는 댓글 반응을 쓰지 않습니다.",
            ));
        }
        let target = self.resolve(ctx, actor, target_type, target_id).await?;
        if who.member_id.is_some() && who.member_id == target.author {
            return Err(module_error(
                Code::Forbidden,
                "자기 글이나 댓글에는 반응할 수 없습니다.",
            ));
        }
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let before = sqlx::query_scalar!(
            "SELECT kind FROM mod_reaction_item
             WHERE target_type = ? AND target_id = ? AND who = ?",
            target_type,
            target_id,
            who.key
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let after = if before.as_deref() == Some(kind) {
            sqlx::query!(
                "DELETE FROM mod_reaction_item
                 WHERE target_type = ? AND target_id = ? AND who = ?",
                target_type,
                target_id,
                who.key
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            None
        } else {
            let at = clock::now();
            sqlx::query!(
                "INSERT INTO mod_reaction_item (target_type, target_id, who, member_id, kind, at)
                 VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (target_type, target_id, who)
                 DO UPDATE SET kind = excluded.kind, at = excluded.at",
                target_type,
                target_id,
                who.key,
                who.member_id,
                kind,
                at
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            Some(kind.to_owned())
        };
        let kind_target = if target_type == "post" {
            Target::new("post", target_id)
        } else {
            Target::new("comment", target_id)
        };
        let audit = AuditRecord::new(
            actor,
            "reaction.press",
            kind_target,
            json!({ "before": before, "after": after }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((after, target.url))
    }

    /// 대상마다 반응 수와 이 사람이 누른 것.
    async fn counts(
        &self,
        target_type: &str,
        ids: &[String],
        kinds: &[(&'static str, &'static str)],
        member_id: Option<&str>,
    ) -> Result<HashMap<String, Vec<KindCount>>> {
        let ids_json = serde_json::to_string(ids).map_err(Error::internal)?;
        let rows = sqlx::query!(
            r#"SELECT target_id, kind, COUNT(*) AS "n!: i64" FROM mod_reaction_item
               WHERE target_type = ? AND target_id IN (SELECT value FROM json_each(?))
               GROUP BY target_id, kind"#,
            target_type,
            ids_json
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let who = member_id.map(|m| format!("m:{m}")).unwrap_or_default();
        let mine = sqlx::query!(
            "SELECT target_id, kind FROM mod_reaction_item
             WHERE target_type = ? AND who = ? AND target_id IN (SELECT value FROM json_each(?))",
            target_type,
            who,
            ids_json
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let mut out = HashMap::new();
        for id in ids {
            let list = kinds
                .iter()
                .map(|(kind, label)| KindCount {
                    id: (*kind).to_owned(),
                    label: (*label).to_owned(),
                    count: rows
                        .iter()
                        .find(|r| &r.target_id == id && r.kind == *kind)
                        .map_or(0, |r| r.n),
                    pressed: mine.iter().any(|r| &r.target_id == id && r.kind == *kind),
                })
                .collect();
            out.insert(id.clone(), list);
        }
        Ok(out)
    }

    /// 손님 해시. 소금은 이 모듈의 표에만 있어 주소를 되짚을 수 없다.
    async fn guest_key(&self, ip: &str, user_agent: &str) -> Result<String> {
        let fresh = auth::new_secret()?;
        sqlx::query!(
            "INSERT INTO mod_reaction_meta (key, value) VALUES ('salt', ?)
             ON CONFLICT (key) DO NOTHING",
            fresh
        )
        .execute(self.db.write_pool())
        .await
        .map_err(Error::internal)?;
        let salt = sqlx::query_scalar!("SELECT value FROM mod_reaction_meta WHERE key = 'salt'")
            .fetch_one(self.db.read_pool())
            .await
            .map_err(Error::internal)?;
        let mut h = Sha256::new();
        h.update(salt.as_bytes());
        h.update([0]);
        h.update(ip.as_bytes());
        h.update([0]);
        h.update(user_agent.as_bytes());
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("g:{hex}"))
    }

    async fn tool_react(&self, ctx: &Ctx, actor: &Actor, input: ReactInput) -> Result<Value> {
        // 회원의 의사 표시다. 에이전트 토큰과 로컬 CLI 는 대신 누르지 않는다.
        let Some(member) = actor.member_id() else {
            return Err(module_error(
                Code::Forbidden,
                "반응은 회원 본인만 남길 수 있습니다.",
            ));
        };
        let who = Who {
            key: format!("m:{member}"),
            member_id: Some(member.clone()),
        };
        let (after, _) = self
            .press(
                ctx,
                actor,
                &who,
                &input.target_type,
                &input.target_id,
                &input.kind,
            )
            .await?;
        Ok(json!({ "kind": after }))
    }

    async fn tool_counts(&self, ctx: &Ctx, actor: &Actor, input: TargetInput) -> Result<Value> {
        self.resolve(ctx, actor, &input.target_type, &input.target_id)
            .await?;
        let settings = settings_of(ctx, self).await?;
        let ids = [input.target_id.clone()];
        let mut map = self
            .counts(
                &input.target_type,
                &ids,
                &kinds_of(&settings),
                actor.member_id().map(String::as_str),
            )
            .await?;
        Ok(json!({ "kinds": map.remove(&input.target_id).unwrap_or_default() }))
    }

    /// 대상의 반응을 모두 지운다 (운영).
    async fn tool_clear(&self, actor: &Actor, input: TargetInput) -> Result<Value> {
        check_type(&input.target_type)?;
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let removed = sqlx::query!(
            "DELETE FROM mod_reaction_item WHERE target_type = ? AND target_id = ?",
            input.target_type,
            input.target_id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        let target = if input.target_type == "post" {
            Target::new("post", &input.target_id)
        } else {
            Target::new("comment", &input.target_id)
        };
        let audit = AuditRecord::new(
            actor,
            "reaction.clear",
            target,
            json!({ "removed": removed }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({ "removed": removed }))
    }
}

#[async_trait]
impl Module for ReactionModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 20261105200001,
            name: "reaction",
            up: include_str!("../migrations/20261105200001_reaction.up.sql"),
            down: include_str!("../migrations/20261105200001_reaction.down.sql"),
        }]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/{target_type}/{target_id}", post(press_form))
                .layer(Extension(Arc::new(self.clone()))),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "mod/reaction/post.html",
                include_str!("../templates/post.html"),
            ),
            (
                "mod/reaction/comment.html",
                include_str!("../templates/comment.html"),
            ),
        ]
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "counts",
                description: "글이나 댓글의 반응 수",
                input_schema: schema_of::<TargetInput>(),
                permission: None,
            },
            ToolDef {
                name: "react",
                description: "회원 본인의 반응을 누른다. 같은 것을 다시 누르면 취소",
                input_schema: schema_of::<ReactInput>(),
                permission: None,
            },
            ToolDef {
                name: "clear",
                description: "대상의 반응을 모두 지운다",
                input_schema: schema_of::<TargetInput>(),
                permission: Some(Permission::PostModerate),
            },
        ]
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        match tool {
            "counts" => self.tool_counts(ctx, actor, parse_input(input)?).await,
            "react" => self.tool_react(ctx, actor, parse_input(input)?).await,
            "clear" => self.tool_clear(actor, parse_input(input)?).await,
            other => Err(maru_modules::tool_not_found(MANIFEST.id, other)),
        }
    }

    /// 탈퇴한 회원의 반응은 남기지 않는다.
    async fn on_event(&self, _ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        if ev.action == "member.withdraw"
            && let Some(member) = &ev.target_id
        {
            sqlx::query!("DELETE FROM mod_reaction_item WHERE member_id = ?", member)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?;
        }
        Ok(())
    }

    fn settings_fields(&self) -> Vec<FieldDef> {
        let field = |key: &str, label: &str, kind: &str, help: &str| FieldDef {
            key: key.to_owned(),
            label: label.to_owned(),
            kind: kind.to_owned(),
            required: kind == "text",
            options: Vec::new(),
            help: Some(help.to_owned()),
        };
        vec![
            field(
                "kinds",
                "반응 종류",
                "text",
                "쉼표로 나눠 적습니다. up, down, heart, laugh, wow, sad",
            ),
            field(
                "comments",
                "댓글 반응",
                "boolean",
                "댓글에도 반응 단추를 보입니다",
            ),
            field(
                "guest",
                "손님 반응",
                "boolean",
                "연결 주소와 브라우저로 사람을 가립니다. 완전한 한 사람 보장은 아닙니다",
            ),
        ]
    }

    fn default_settings(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("kinds".into(), json!(DEFAULT_KINDS));
        m.insert("comments".into(), json!(true));
        m.insert("guest".into(), json!(false));
        m
    }

    fn check_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        let text = settings.get("kinds").and_then(Value::as_str).unwrap_or("");
        let unknown: Vec<&str> = text
            .split(',')
            .map(str::trim)
            .filter(|k| !KINDS.iter().any(|(id, _)| id == k))
            .collect();
        if !unknown.is_empty() {
            return Err(module_error(
                Code::Validation,
                format!(
                    "모르는 반응 종류입니다: {}. up, down, heart, laugh, wow, sad 중에서 고르세요.",
                    unknown.join(", ")
                ),
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
        let member = view.viewer.member_id().map(String::as_str);
        match slot {
            POST_FOOTER => {
                let Some(post_id) = view.post_id else {
                    return Ok(None);
                };
                let settings = settings_of(ctx, self).await?;
                let ids = [post_id.to_owned()];
                let mut map = self
                    .counts("post", &ids, &kinds_of(&settings), member)
                    .await?;
                Ok(Some(SlotContent {
                    template: "mod/reaction/post.html",
                    data: json!({
                        "action": format!("/m/reaction/post/{post_id}"),
                        "kinds": map.remove(post_id).unwrap_or_default(),
                    }),
                }))
            }
            POST_LIST => {
                if view.post_ids.is_empty() {
                    return Ok(None);
                }
                let settings = settings_of(ctx, self).await?;
                // 목록엔 첫 종류(기본은 추천)의 수만 둔다. 추천·비추천을 더하면 뜻이 흐려진다.
                let kinds = kinds_of(&settings);
                let Some(&(first, label)) = kinds.first() else {
                    return Ok(None);
                };
                // 목록은 모두에게 같은 HTML 이라 누른 사람은 보지 않는다.
                let map = self
                    .counts("post", view.post_ids, &kinds[..1], None)
                    .await?;
                let by_id: Map<String, Value> = map
                    .into_iter()
                    .filter_map(|(id, ks)| {
                        let n = ks.iter().find(|k| k.id == first).map_or(0, |k| k.count);
                        (n > 0).then(|| (id, json!(n)))
                    })
                    .collect();
                Ok(Some(SlotContent {
                    template: "",
                    data: json!({ "label": label, "by_id": by_id }),
                }))
            }
            COMMENT_ACTIONS => {
                let settings = settings_of(ctx, self).await?;
                if view.comment_ids.is_empty() || !flag(&settings, "comments") {
                    return Ok(None);
                }
                let map = self
                    .counts("comment", view.comment_ids, &kinds_of(&settings), member)
                    .await?;
                Ok(Some(SlotContent {
                    template: "mod/reaction/comment.html",
                    data: json!({ "by_id": map }),
                }))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct PressForm {
    #[serde(default)]
    kind: String,
}

/// 공개 글 화면의 단추. 자바스크립트 없이 동작한다.
async fn press_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<ReactionModule>>,
    Path((target_type, target_id)): Path<(String, String)>,
    headers: HeaderMap,
    extensions: Extensions,
    Form(f): Form<PressForm>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    let who = match actor.member_id() {
        Some(m) => Who {
            key: format!("m:{m}"),
            member_id: Some(m.clone()),
        },
        None => {
            let settings = match settings_of(&state.ctx, module.as_ref()).await {
                Ok(s) => s,
                Err(e) => return state.host.error(&e).await,
            };
            if !flag(&settings, "guest") {
                // 손님 반응을 안 받으면 로그인으로 보낸다. 돌아올 주소는 대상에서 찾는다.
                let target = module
                    .resolve(&state.ctx, &actor, &target_type, &target_id)
                    .await;
                return match target {
                    Ok(t) => Redirect::to(&urls::with_query("/login", &[("next", t.url.as_str())]))
                        .into_response(),
                    Err(e) => state.host.error(&e).await,
                };
            }
            let ip = extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip().to_string())
                .unwrap_or_default();
            let ua = headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            match module.guest_key(&ip, ua).await {
                Ok(key) => Who {
                    key,
                    member_id: None,
                },
                Err(e) => return state.host.error(&e).await,
            }
        }
    };
    match module
        .press(&state.ctx, &actor, &who, &target_type, &target_id, &f.kind)
        .await
    {
        Ok((_, url)) => Redirect::to(&url).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}
