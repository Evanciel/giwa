//! 투표 모듈 (GAP M14, ADR-0070). 글 하나에 투표 하나를 붙인다.
//!
//! - 만들기·마감·지우기는 글 작성자 또는 그 게시판을 관리할 수 있는 사람만.
//! - 투표는 회원만, 볼 수 있는 글에서만, 마감 전까지 바꿀 수 있다. 다시 하면 이전 선택을 대신한다.
//! - 막대는 `<progress>` 로 그린다. CSP 가 인라인 스타일을 막기 때문이다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use maru_core::actor::{Actor, Permission};
use maru_core::audit::{AuditRecord, Target};
use maru_core::fields::FieldDef;
use maru_core::model::{AuditEvent, Post};
use maru_core::services::{posts, settings};
use maru_core::store::ModuleMigration;
use maru_core::urls::{self, Target as UrlTarget};
use maru_core::{Code, Ctx, Error, Result, clock};
use maru_modules::service::settings_of;
use maru_modules::{
    Manifest, Module, ModuleState, POST_FOOTER, SlotContent, SlotView, ToolDef, module_error,
    schema_of,
};
use maru_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

static MANIFEST: Manifest = Manifest {
    id: "poll",
    name: "투표",
    version: env!("CARGO_PKG_VERSION"),
    description: "글에 투표를 붙이고 회원의 표를 모읍니다.",
    events: &["member.withdraw"],
};

const MAX_QUESTION: usize = 200;
const MAX_LABEL: usize = 100;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 10;

#[derive(Clone)]
pub struct PollModule {
    db: Db,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OptionView {
    pub id: String,
    pub label: String,
    /// 결과를 가릴 때는 비어 있다
    pub votes: Option<i64>,
    pub pressed: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PollView {
    pub id: String,
    pub post_id: String,
    pub question: String,
    pub multiple: bool,
    /// 사이트 시간대로 적은 마감 시각
    pub closes_at: Option<String>,
    pub open: bool,
    pub voters: i64,
    pub voted: bool,
    /// 결과를 보여 줘도 되는가
    pub results: bool,
    pub options: Vec<OptionView>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    post: String,
    question: String,
    /// 2~10개. `--options 첫째 --options 둘째`
    options: Vec<String>,
    /// 여러 개 고를 수 있게 한다
    #[serde(default)]
    multiple: bool,
    /// 마감 시각 (RFC3339 또는 `YYYY-MM-DDTHH:MM`, 사이트 시간대)
    #[serde(default)]
    closes_at: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PostInput {
    post: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VoteInput {
    post: String,
    /// 고른 보기의 ID
    options: Vec<String>,
}

/// 표의 한 줄.
struct RawPoll {
    id: String,
    post_id: String,
    question: String,
    multiple: i64,
    closes_at: Option<String>,
    closed_at: Option<String>,
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn no_post() -> Error {
    module_error(Code::NotFound, "글을 찾을 수 없습니다.")
}

fn no_poll() -> Error {
    module_error(Code::NotFound, "이 글에는 투표가 없습니다.")
}

fn invalid(text: impl Into<String>) -> Error {
    module_error(Code::Validation, text)
}

impl RawPoll {
    fn is_open(&self, now: &str) -> bool {
        self.closed_at.is_none() && self.closes_at.as_deref().is_none_or(|c| c > now)
    }
}

impl PollModule {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    async fn by_post(&self, post_id: &str) -> Result<Option<RawPoll>> {
        sqlx::query_as!(
            RawPoll,
            "SELECT id, post_id, question, multiple, closes_at, closed_at
             FROM mod_poll WHERE post_id = ?",
            post_id
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)
    }

    /// 글 작성자이거나 그 게시판을 관리할 수 있는가.
    async fn can_manage(&self, ctx: &Ctx, actor: &Actor, post: &Post) -> Result<bool> {
        if post.author_id.is_some() && actor.member_id() == post.author_id.as_ref() {
            return Ok(true);
        }
        let Some(board) = ctx.store.board_by_id(&post.board_id).await? else {
            return Ok(false);
        };
        Ok(actor.has(Permission::PostModerate)
            && actor
                .require_rank(Permission::PostModerate, &board.policy.moderate)
                .is_ok()
            && actor
                .require_board(Permission::PostModerate, &board.id)
                .is_ok())
    }

    async fn manageable(&self, ctx: &Ctx, actor: &Actor, post_id: &str) -> Result<Post> {
        let post = ctx.store.post_by_id(post_id).await?.ok_or_else(no_post)?;
        if !self.can_manage(ctx, actor, &post).await? {
            return Err(module_error(
                Code::Forbidden,
                "글 작성자나 게시판 관리자만 투표를 다룰 수 있습니다.",
            ));
        }
        Ok(post)
    }

    /// 이 사람이 지금 볼 수 있는 글의 주소.
    async fn visible_url(&self, ctx: &Ctx, actor: &Actor, post_id: &str) -> Result<String> {
        let post = ctx.store.post_by_id(post_id).await?.ok_or_else(no_post)?;
        let board = ctx
            .store
            .board_by_id(&post.board_id)
            .await?
            .ok_or_else(no_post)?;
        let (board, post) = posts::public_post(ctx, actor, &board.slug, &post.slug).await?;
        Ok(urls::path(&UrlTarget::Post {
            board: &board.slug,
            post: &post.slug,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    async fn create(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        post_id: &str,
        question: &str,
        options: &[String],
        multiple: bool,
        closes_at: Option<&str>,
    ) -> Result<String> {
        let post = self.manageable(ctx, actor, post_id).await?;
        let question = question.trim();
        if question.is_empty() || question.chars().count() > MAX_QUESTION {
            return Err(invalid(format!("질문은 1~{MAX_QUESTION}자로 적어 주세요.")));
        }
        let mut labels: Vec<&str> = Vec::new();
        for label in options.iter().map(|o| o.trim()).filter(|o| !o.is_empty()) {
            if label.chars().count() > MAX_LABEL {
                return Err(invalid(format!(
                    "보기는 {MAX_LABEL}자까지 적을 수 있습니다."
                )));
            }
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&labels.len()) {
            return Err(invalid(format!(
                "서로 다른 보기를 {MIN_OPTIONS}~{MAX_OPTIONS}개 적어 주세요."
            )));
        }
        let tz = settings::site(ctx).await?.tz_offset_min;
        let closes_at = match closes_at.map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => {
                let at = clock::from_local_input(raw, tz)
                    .ok_or_else(|| invalid("마감 시각의 모양이 맞지 않습니다."))?;
                if at.as_str() <= clock::now().as_str() {
                    return Err(invalid("마감 시각은 지금보다 뒤여야 합니다."));
                }
                Some(at)
            }
            None => None,
        };
        if self.by_post(&post.id).await?.is_some() {
            return Err(module_error(
                Code::Conflict,
                "이 글에는 이미 투표가 있습니다. 지운 뒤 다시 만드세요.",
            ));
        }
        let id = clock::new_id();
        let at = clock::now();
        let is_multiple = i64::from(multiple);
        let created_by = actor.member_id().cloned();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "INSERT INTO mod_poll (id, post_id, question, multiple, closes_at, created_by, at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            id,
            post.id,
            question,
            is_multiple,
            closes_at,
            created_by,
            at
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        for (ord, label) in labels.iter().enumerate() {
            let option_id = clock::new_id();
            let ord = i64::try_from(ord).map_err(Error::internal)?;
            sqlx::query!(
                "INSERT INTO mod_poll_option (id, poll_id, label, ord) VALUES (?, ?, ?, ?)",
                option_id,
                id,
                label,
                ord
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let audit = AuditRecord::new(
            actor,
            "poll.create",
            Target::new("post", &post.id),
            json!({ "poll": id, "question": question, "options": labels, "multiple": multiple }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(id)
    }

    /// 표를 넣는다. 이전 선택은 대신한다.
    async fn vote(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        post_id: &str,
        chosen: &[String],
    ) -> Result<String> {
        let Some(member) = actor.member_id() else {
            return Err(module_error(
                Code::Forbidden,
                "투표는 회원만 할 수 있습니다.",
            ));
        };
        let url = self.visible_url(ctx, actor, post_id).await?;
        let poll = self.by_post(post_id).await?.ok_or_else(no_poll)?;
        let now = clock::now();
        if !poll.is_open(&now) {
            return Err(invalid("마감된 투표입니다."));
        }
        let valid: BTreeSet<String> =
            sqlx::query_scalar!("SELECT id FROM mod_poll_option WHERE poll_id = ?", poll.id)
                .fetch_all(self.db.read_pool())
                .await
                .map_err(Error::internal)?
                .into_iter()
                .collect();
        let chosen: BTreeSet<&String> = chosen.iter().collect();
        if chosen.is_empty() {
            return Err(invalid("보기를 하나 이상 고르세요."));
        }
        if poll.multiple == 0 && chosen.len() > 1 {
            return Err(invalid("이 투표는 하나만 고를 수 있습니다."));
        }
        if chosen.iter().any(|c| !valid.contains(*c)) {
            return Err(invalid("이 투표의 보기가 아닙니다."));
        }
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "DELETE FROM mod_poll_vote WHERE poll_id = ? AND member_id = ?",
            poll.id,
            member
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        for option in &chosen {
            sqlx::query!(
                "INSERT INTO mod_poll_vote (poll_id, option_id, member_id, at) VALUES (?, ?, ?, ?)",
                poll.id,
                option,
                member,
                now
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        // 무엇을 골랐는지는 감사 기록에 남기지 않는다.
        let audit = AuditRecord::new(
            actor,
            "poll.vote",
            Target::new("post", post_id),
            json!({ "poll": poll.id, "count": chosen.len() }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(url)
    }

    async fn close(&self, ctx: &Ctx, actor: &Actor, post_id: &str) -> Result<()> {
        let post = self.manageable(ctx, actor, post_id).await?;
        let now = clock::now();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let done = sqlx::query!(
            "UPDATE mod_poll SET closed_at = COALESCE(closed_at, ?) WHERE post_id = ?",
            now,
            post.id
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if done == 0 {
            return Err(no_poll());
        }
        let audit = AuditRecord::new(
            actor,
            "poll.close",
            Target::new("post", &post.id),
            json!({}),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)
    }

    async fn remove(&self, ctx: &Ctx, actor: &Actor, post_id: &str) -> Result<()> {
        let post = self.manageable(ctx, actor, post_id).await?;
        let poll = self.by_post(&post.id).await?.ok_or_else(no_poll)?;
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!("DELETE FROM mod_poll_vote WHERE poll_id = ?", poll.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query!("DELETE FROM mod_poll_option WHERE poll_id = ?", poll.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query!("DELETE FROM mod_poll WHERE id = ?", poll.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let audit = AuditRecord::new(
            actor,
            "poll.delete",
            Target::new("post", &post.id),
            json!({ "poll": poll.id, "question": poll.question }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)
    }

    /// 사이트 설정이 켜져 있으면 표를 던지거나 마감된 뒤에만 결과를 보여 준다. 관리하는 사람은 언제나 본다.
    async fn view(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        poll: RawPoll,
        manager: bool,
    ) -> Result<PollView> {
        let options = sqlx::query!(
            "SELECT id, label FROM mod_poll_option WHERE poll_id = ? ORDER BY ord",
            poll.id
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let counts: BTreeMap<String, i64> = sqlx::query!(
            r#"SELECT option_id, COUNT(*) AS "n!: i64" FROM mod_poll_vote
               WHERE poll_id = ? GROUP BY option_id"#,
            poll.id
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?
        .into_iter()
        .map(|r| (r.option_id, r.n))
        .collect();
        let voters = sqlx::query_scalar!(
            r#"SELECT COUNT(DISTINCT member_id) AS "n!: i64" FROM mod_poll_vote WHERE poll_id = ?"#,
            poll.id
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let mine: BTreeSet<String> = match actor.member_id() {
            Some(m) => sqlx::query_scalar!(
                "SELECT option_id FROM mod_poll_vote WHERE poll_id = ? AND member_id = ?",
                poll.id,
                m
            )
            .fetch_all(self.db.read_pool())
            .await
            .map_err(Error::internal)?
            .into_iter()
            .collect(),
            None => BTreeSet::new(),
        };
        let after_vote = settings_of(ctx, self)
            .await?
            .get("results_after_vote")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let open = poll.is_open(&clock::now());
        let voted = !mine.is_empty();
        let results = !after_vote || voted || !open || manager;
        let tz = settings::site(ctx).await?.tz_offset_min;
        Ok(PollView {
            options: options
                .into_iter()
                .map(|o| OptionView {
                    votes: results.then(|| counts.get(&o.id).copied().unwrap_or(0)),
                    pressed: mine.contains(&o.id),
                    id: o.id,
                    label: o.label,
                })
                .collect(),
            closes_at: poll
                .closes_at
                .as_deref()
                .map(|c| clock::display_datetime(c, tz)),
            id: poll.id,
            post_id: poll.post_id,
            question: poll.question,
            multiple: poll.multiple != 0,
            open,
            voters,
            voted,
            results,
        })
    }

    async fn tool_get(&self, ctx: &Ctx, actor: &Actor, input: PostInput) -> Result<Value> {
        self.visible_url(ctx, actor, &input.post).await?;
        let post = ctx
            .store
            .post_by_id(&input.post)
            .await?
            .ok_or_else(no_post)?;
        let manager = self.can_manage(ctx, actor, &post).await?;
        let poll = self.by_post(&input.post).await?.ok_or_else(no_poll)?;
        Ok(json!(self.view(ctx, actor, poll, manager).await?))
    }
}

#[async_trait]
impl Module for PollModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 20261105600001,
            name: "poll",
            up: include_str!("../migrations/20261105600001_poll.up.sql"),
            down: include_str!("../migrations/20261105600001_poll.down.sql"),
        }]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/new/{post}", get(new_page).post(create_form))
                .route("/vote/{post}", post(vote_form))
                .route("/close/{post}", post(close_form))
                .route("/delete/{post}", post(delete_form))
                .layer(Extension(Arc::new(self.clone()))),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("mod/poll/post.html", include_str!("../templates/post.html")),
            ("mod/poll/new.html", include_str!("../templates/new.html")),
        ]
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "create",
                description: "글에 투표를 붙인다. 글 작성자 또는 게시판 관리자",
                input_schema: schema_of::<CreateInput>(),
                permission: None,
            },
            ToolDef {
                name: "get",
                description: "글의 투표와 결과",
                input_schema: schema_of::<PostInput>(),
                permission: None,
            },
            ToolDef {
                name: "vote",
                description: "회원 본인의 표를 넣는다. 다시 하면 이전 선택을 대신한다",
                input_schema: schema_of::<VoteInput>(),
                permission: None,
            },
            ToolDef {
                name: "close",
                description: "투표를 마감한다",
                input_schema: schema_of::<PostInput>(),
                permission: None,
            },
            ToolDef {
                name: "delete",
                description: "투표와 표를 지운다",
                input_schema: schema_of::<PostInput>(),
                permission: None,
            },
        ]
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        match tool {
            "create" => {
                let input: CreateInput = parse_input(input)?;
                let id = self
                    .create(
                        ctx,
                        actor,
                        &input.post,
                        &input.question,
                        &input.options,
                        input.multiple,
                        input.closes_at.as_deref(),
                    )
                    .await?;
                self.tool_get(ctx, actor, PostInput { post: input.post })
                    .await
                    .map(|mut v| {
                        v["id"] = json!(id);
                        v
                    })
            }
            "get" => self.tool_get(ctx, actor, parse_input(input)?).await,
            "vote" => {
                let input: VoteInput = parse_input(input)?;
                self.vote(ctx, actor, &input.post, &input.options).await?;
                self.tool_get(ctx, actor, PostInput { post: input.post })
                    .await
            }
            "close" => {
                let input: PostInput = parse_input(input)?;
                self.close(ctx, actor, &input.post).await?;
                self.tool_get(ctx, actor, input).await
            }
            "delete" => {
                let input: PostInput = parse_input(input)?;
                self.remove(ctx, actor, &input.post).await?;
                Ok(json!({ "deleted": input.post }))
            }
            other => Err(maru_modules::tool_not_found(MANIFEST.id, other)),
        }
    }

    /// 탈퇴한 회원의 표는 남기지 않는다.
    async fn on_event(&self, _ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        if ev.action == "member.withdraw"
            && let Some(member) = &ev.target_id
        {
            sqlx::query!("DELETE FROM mod_poll_vote WHERE member_id = ?", member)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?;
        }
        Ok(())
    }

    fn settings_fields(&self) -> Vec<FieldDef> {
        vec![FieldDef {
            key: "results_after_vote".to_owned(),
            label: "표를 던진 뒤에 결과 보이기".to_owned(),
            kind: "boolean".to_owned(),
            required: false,
            options: Vec::new(),
            help: Some("끄면 누구나 언제든 결과를 봅니다.".to_owned()),
        }]
    }

    fn default_settings(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("results_after_vote".into(), json!(true));
        m
    }

    async fn slot(
        &self,
        ctx: &Ctx,
        slot: &str,
        view: &SlotView<'_>,
    ) -> Result<Option<SlotContent>> {
        if slot != POST_FOOTER {
            return Ok(None);
        }
        let Some(post_id) = view.post_id else {
            return Ok(None);
        };
        let Some(post) = ctx.store.post_by_id(post_id).await? else {
            return Ok(None);
        };
        let manager = self.can_manage(ctx, view.viewer, &post).await?;
        let Some(poll) = self.by_post(post_id).await? else {
            // 투표가 없으면 다룰 수 있는 사람에게만 만들기 단추를 보인다.
            if !manager {
                return Ok(None);
            }
            return Ok(Some(SlotContent {
                template: "mod/poll/post.html",
                data: json!({
                    "poll": null,
                    "new_url": format!("/m/poll/new/{post_id}"),
                }),
            }));
        };
        let member = view.viewer.member_id().is_some();
        let pv = self.view(ctx, view.viewer, poll, manager).await?;
        let total = pv
            .options
            .iter()
            .filter_map(|o| o.votes)
            .sum::<i64>()
            .max(1);
        Ok(Some(SlotContent {
            template: "mod/poll/post.html",
            data: json!({
                "poll": pv,
                "total": total,
                "member": member,
                "manager": manager,
                "post_id": post_id,
            }),
        }))
    }
}

/// 회원이 아닌 사람을 돌려보낼 로그인 주소.
fn to_login(next: &str) -> Response {
    Redirect::to(&urls::with_query("/login", &[("next", next)])).into_response()
}

/// 폼의 앞머리: 같은 사이트에서 왔는가, 지금 누구인가.
async fn poster(
    state: &ModuleState,
    headers: &HeaderMap,
) -> std::result::Result<Actor, Box<Response>> {
    if !state.host.same_origin(headers) {
        return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
    }
    match state.host.viewer(headers).await {
        Ok(a) => Ok(a),
        Err(e) => Err(Box::new(state.host.error(&e).await)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn compose(
    state: &ModuleState,
    post_id: &str,
    title: &str,
    question: &str,
    options: &str,
    multiple: bool,
    closes_at: &str,
    error: Option<String>,
) -> Response {
    state
        .host
        .page(
            "mod/poll/new.html",
            "투표 만들기",
            json!({
                "post_id": post_id,
                "post_title": title,
                "question": question,
                "options": options,
                "multiple": multiple,
                "closes_at": closes_at,
                "error": error,
                "noindex": true,
            }),
        )
        .await
}

async fn new_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PollModule>>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    if actor.member_id().is_none() && !actor.has(Permission::PostModerate) {
        return to_login(&format!("/m/poll/new/{post_id}"));
    }
    match module.manageable(&state.ctx, &actor, &post_id).await {
        Ok(post) => compose(&state, &post_id, &post.title, "", "", false, "", None).await,
        Err(e) => state.host.error(&e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct CreateForm {
    #[serde(default)]
    question: String,
    /// 줄마다 보기 하나
    #[serde(default)]
    options: String,
    #[serde(default)]
    multiple: Option<String>,
    #[serde(default)]
    closes_at: String,
}

async fn create_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PollModule>>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<CreateForm>,
) -> Response {
    let actor = match poster(&state, &headers).await {
        Ok(a) => a,
        Err(res) => return *res,
    };
    let options: Vec<String> = f.options.lines().map(str::to_owned).collect();
    let multiple = f.multiple.is_some();
    match module
        .create(
            &state.ctx,
            &actor,
            &post_id,
            &f.question,
            &options,
            multiple,
            Some(&f.closes_at),
        )
        .await
    {
        Ok(_) => match module.visible_url(&state.ctx, &actor, &post_id).await {
            Ok(url) => Redirect::to(&format!("{url}#reactions")).into_response(),
            Err(e) => state.host.error(&e).await,
        },
        Err(e) if e.code() == Code::Validation => {
            let title = match state.ctx.store.post_by_id(&post_id).await {
                Ok(Some(p)) => p.title,
                _ => String::new(),
            };
            compose(
                &state,
                &post_id,
                &title,
                &f.question,
                &f.options,
                multiple,
                &f.closes_at,
                Some(e.to_string()),
            )
            .await
        }
        Err(e) => state.host.error(&e).await,
    }
}

/// `option=<id>` 가 여러 번 올 수 있어 짝의 목록으로 받는다.
async fn vote_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PollModule>>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let actor = match poster(&state, &headers).await {
        Ok(a) => a,
        Err(res) => return *res,
    };
    if actor.member_id().is_none() {
        return match module.visible_url(&state.ctx, &actor, &post_id).await {
            Ok(url) => to_login(&url),
            Err(e) => state.host.error(&e).await,
        };
    }
    let chosen: Vec<String> = pairs
        .into_iter()
        .filter(|(k, _)| k == "option")
        .map(|(_, v)| v)
        .collect();
    match module.vote(&state.ctx, &actor, &post_id, &chosen).await {
        Ok(url) => Redirect::to(&format!("{url}#reactions")).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}

async fn close_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PollModule>>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match poster(&state, &headers).await {
        Ok(a) => a,
        Err(res) => return *res,
    };
    let done = async {
        module.close(&state.ctx, &actor, &post_id).await?;
        module.visible_url(&state.ctx, &actor, &post_id).await
    }
    .await;
    match done {
        Ok(url) => Redirect::to(&format!("{url}#reactions")).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}

async fn delete_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PollModule>>,
    Path(post_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match poster(&state, &headers).await {
        Ok(a) => a,
        Err(res) => return *res,
    };
    let done = async {
        module.remove(&state.ctx, &actor, &post_id).await?;
        module.visible_url(&state.ctx, &actor, &post_id).await
    }
    .await;
    match done {
        Ok(url) => Redirect::to(&format!("{url}#reactions")).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}
