//! 포인트 모듈 (GAP M13, SPEC Phase 2 샘플, ADR-0067).
//!
//! 글·댓글·로그인(하루 한 번)에 적립하고, 글·댓글이 지워지면 준 만큼 되돌린다. 모든 증감은 원장에 한 줄씩
//! 남고 `source` 가 유일해서 같은 이벤트가 다시 와도 두 번 적립되지 않는다.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use maru_core::actor::{Actor, Permission};
use maru_core::audit::{AuditRecord, Target};
use maru_core::fields::FieldDef;
use maru_core::model::{AuditEvent, CommentStatus, Member, MemberStatus, PostStatus};
use maru_core::services::widgets::WidgetType;
use maru_core::store::ModuleMigration;
use maru_core::{Code, Ctx, Error, Result, clock};
use maru_modules::service::settings_of;
use maru_modules::{
    MEMBER_BADGE, Manifest, Module, ModuleState, SlotContent, SlotView, ToolDef, module_error,
    schema_of,
};
use maru_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

static MANIFEST: Manifest = Manifest {
    id: "point",
    name: "포인트",
    version: env!("CARGO_PKG_VERSION"),
    description: "글·댓글·로그인에 포인트를 주고 레벨과 순위를 보여 줍니다.",
    events: &[
        "post.write",
        "post.publish",
        "post.delete",
        "comment.create",
        "comment.approve",
        "comment.delete",
        "auth.login",
    ],
};

const WIDGET: &str = "point_ranking";
/// 하루를 가르는 시간대(한국 시간, 분).
const DAY_OFFSET_MINUTES: i32 = 540;
const MAX_RANKING: i64 = 100;
const HISTORY_PER_PAGE: i64 = 30;
const MAX_REASON: usize = 200;
/// 레벨 기준 기본값: 이 점수를 넘을 때마다 한 단계 오른다.
const DEFAULT_LEVELS: &str = "100,500,2000,10000";
/// 레벨 이름 한 칸의 최대 글자 수. 이름 옆에 붙어 길면 목록이 밀린다.
const MAX_LEVEL_NAME: usize = 8;

#[derive(Clone)]
pub struct PointModule {
    db: Db,
}

impl PointModule {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 한 건 적립한다. `source` 가 이미 있으면 아무것도 하지 않고 거짓을 돌려준다.
    async fn credit(
        &self,
        member_id: &str,
        amount: i64,
        reason: &str,
        source: &str,
        audit: Option<AuditRecord>,
    ) -> Result<bool> {
        if amount == 0 {
            return Ok(false);
        }
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let id = clock::new_id();
        let at = clock::now();
        let inserted = sqlx::query!(
            "INSERT OR IGNORE INTO mod_point_ledger (id, member_id, amount, reason, source, at)
             VALUES (?, ?, ?, ?, ?, ?)",
            id,
            member_id,
            amount,
            reason,
            source,
            at
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected()
            == 1;
        if inserted {
            sqlx::query!(
                "INSERT INTO mod_point_balance (member_id, balance) VALUES (?, ?)
                 ON CONFLICT (member_id) DO UPDATE SET balance = balance + excluded.balance",
                member_id,
                amount
            )
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if let Some(rec) = &audit {
                maru_storage::sqlite::insert_audit(&mut tx, rec).await?;
            }
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(inserted)
    }

    /// 이 열쇠로 적립된 것을 되돌린다 (글·댓글이 지워졌을 때). 받은 사람에게서 뺀다.
    async fn undo(&self, source: &str, reason: &str) -> Result<()> {
        let row = sqlx::query!(
            "SELECT member_id, amount FROM mod_point_ledger WHERE source = ?",
            source
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        if let Some(r) = row {
            self.credit(
                &r.member_id,
                -r.amount,
                reason,
                &format!("undo:{source}"),
                None,
            )
            .await?;
        }
        Ok(())
    }

    async fn balance_of(&self, member_id: &str) -> Result<i64> {
        let row = sqlx::query!(
            "SELECT balance FROM mod_point_balance WHERE member_id = ?",
            member_id
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(row.map_or(0, |r| r.balance))
    }

    /// 여러 회원의 잔액. 원장에 없는 회원은 빠진다(0 점).
    async fn balances_of(&self, ids: &[String]) -> Result<std::collections::HashMap<String, i64>> {
        let ids_json = serde_json::to_string(ids).map_err(Error::internal)?;
        let rows = sqlx::query!(
            r#"SELECT member_id AS "member_id!: String", balance FROM mod_point_balance
               WHERE member_id IN (SELECT value FROM json_each(?))"#,
            ids_json
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|r| (r.member_id, r.balance)).collect())
    }

    /// 순위. 사용 중이 아닌 회원은 건너뛴다.
    async fn ranking(&self, ctx: &Ctx, limit: i64) -> Result<Vec<RankRow>> {
        let limit = limit.clamp(1, MAX_RANKING);
        let levels = levels_of(&settings_of(ctx, self).await?);
        // 멈춘 회원을 걸러도 모자라지 않게 조금 더 읽는다.
        let fetch = limit + 20;
        let rows = sqlx::query!(
            "SELECT member_id, balance FROM mod_point_balance
             WHERE balance > 0 ORDER BY balance DESC, member_id LIMIT ?",
            fetch
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for r in rows {
            let Some(member) = ctx.store.member_by_id(&r.member_id).await? else {
                continue;
            };
            if member.status != MemberStatus::Active {
                continue;
            }
            out.push(RankRow {
                rank: out.len() as i64 + 1,
                member_id: r.member_id,
                name: member.display_name,
                balance: r.balance,
                level: level_for(r.balance, &levels),
            });
            if out.len() as i64 == limit {
                break;
            }
        }
        Ok(out)
    }

    async fn on_post(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        let Some(id) = &ev.target_id else {
            return Ok(());
        };
        let Some(post) = ctx.store.post_by_id(id).await? else {
            return Ok(());
        };
        // 승인 대기·초안은 발행될 때 준다. 받는 사람은 발행한 사람이 아니라 글쓴이다.
        if post.status != PostStatus::Published {
            return Ok(());
        }
        let Some(author) = post.author_id else {
            return Ok(());
        };
        let amount = number(&settings_of(ctx, self).await?, "post");
        self.credit(&author, amount, "글 작성", &format!("post:{id}"), None)
            .await?;
        Ok(())
    }

    async fn on_comment(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        let Some(id) = &ev.target_id else {
            return Ok(());
        };
        let Some(comment) = ctx.store.comment_by_id(id).await? else {
            return Ok(());
        };
        if comment.status != CommentStatus::Published {
            return Ok(());
        }
        let Some(author) = comment.author_id else {
            return Ok(());
        };
        let amount = number(&settings_of(ctx, self).await?, "comment");
        self.credit(&author, amount, "댓글 작성", &format!("comment:{id}"), None)
            .await?;
        Ok(())
    }

    async fn on_login(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        let Some(member) = &ev.target_id else {
            return Ok(());
        };
        let day = clock::display_date(&ev.at, DAY_OFFSET_MINUTES);
        let amount = number(&settings_of(ctx, self).await?, "login");
        self.credit(
            member,
            amount,
            "로그인",
            &format!("login:{member}:{day}"),
            None,
        )
        .await?;
        Ok(())
    }

    async fn tool_balance(&self, ctx: &Ctx, actor: &Actor, input: BalanceInput) -> Result<Value> {
        let member = target_member(ctx, actor, input.member.as_deref()).await?;
        let balance = self.balance_of(&member.id).await?;
        let levels = levels_of(&settings_of(ctx, self).await?);
        Ok(json!({
            "member_id": member.id,
            "name": member.display_name,
            "balance": balance,
            "level": level_for(balance, &levels),
        }))
    }

    async fn tool_history(&self, ctx: &Ctx, actor: &Actor, input: HistoryInput) -> Result<Value> {
        let member = target_member(ctx, actor, input.member.as_deref()).await?;
        if actor.member_id() != Some(&member.id) {
            actor.require(Permission::MemberManage)?;
        }
        let page = input.page.unwrap_or(1).max(1);
        let offset = (page - 1) * HISTORY_PER_PAGE;
        let rows = sqlx::query!(
            "SELECT amount, reason, at FROM mod_point_ledger
             WHERE member_id = ? ORDER BY at DESC, id DESC LIMIT ? OFFSET ?",
            member.id,
            HISTORY_PER_PAGE,
            offset
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let items: Vec<Value> = rows
            .into_iter()
            .map(|r| json!({ "amount": r.amount, "reason": r.reason, "at": r.at }))
            .collect();
        Ok(json!({ "member_id": member.id, "page": page, "items": items }))
    }

    async fn tool_adjust(&self, ctx: &Ctx, actor: &Actor, input: AdjustInput) -> Result<Value> {
        let member = find_member(ctx, &input.member).await?;
        if input.amount == 0 {
            return Err(module_error(
                Code::Validation,
                "금액은 0 이 아니어야 합니다.",
            ));
        }
        let reason = input.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON {
            return Err(module_error(
                Code::Validation,
                format!("사유는 1~{MAX_REASON}자로 적으세요."),
            ));
        }
        let audit = AuditRecord::new(
            actor,
            "point.adjust",
            Target::new("member", &member.id),
            json!({ "amount": input.amount, "reason": reason }),
        );
        let source = format!("adjust:{}", clock::new_id());
        self.credit(&member.id, input.amount, reason, &source, Some(audit))
            .await?;
        Ok(json!({ "member_id": member.id, "balance": self.balance_of(&member.id).await? }))
    }
}

#[derive(Debug, Clone, Serialize)]
struct RankRow {
    rank: i64,
    member_id: String,
    name: String,
    balance: i64,
    level: i64,
}

/// 포인트 조회.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BalanceInput {
    /// 회원 ID 또는 이메일. 비우면 나
    #[serde(default)]
    member: Option<String>,
}

/// 포인트 내역.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryInput {
    /// 회원 ID 또는 이메일. 비우면 나. 남의 내역은 회원 관리 권한이 있어야 본다
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    page: Option<i64>,
}

/// 포인트 순위.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RankingInput {
    /// 몇 명까지 (1~100, 기본 10)
    #[serde(default)]
    limit: Option<i64>,
}

/// 포인트 손으로 고치기.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AdjustInput {
    /// 회원 ID 또는 이메일
    member: String,
    /// 더할 점수. 뺄 때는 음수
    amount: i64,
    /// 사유
    reason: String,
}

/// 포인트 순위 위젯 설정.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RankingWidget {
    /// 몇 명까지 (1~20)
    #[serde(default = "default_widget_limit")]
    limit: i64,
}

fn default_widget_limit() -> i64 {
    5
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

async fn find_member(ctx: &Ctx, key: &str) -> Result<Member> {
    let found = if key.contains('@') {
        ctx.store.member_by_email(key).await?
    } else {
        ctx.store.member_by_id(key).await?
    };
    found.ok_or_else(|| module_error(Code::NotFound, format!("회원을 찾을 수 없습니다: {key}")))
}

async fn target_member(ctx: &Ctx, actor: &Actor, key: Option<&str>) -> Result<Member> {
    match (key.filter(|k| !k.trim().is_empty()), actor.member_id()) {
        (Some(k), _) => find_member(ctx, k.trim()).await,
        (None, Some(me)) => find_member(ctx, me).await,
        (None, None) => Err(module_error(Code::Validation, "--member 를 적으세요.")),
    }
}

fn number(settings: &Map<String, Value>, key: &str) -> i64 {
    settings.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// 레벨 기준을 읽는다. 잘못된 조각은 건너뛴다(저장할 때 이미 검사했다).
fn levels_of(settings: &Map<String, Value>) -> Vec<i64> {
    settings
        .get("levels")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_LEVELS)
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// 레벨 이름 (GAP Q1). 비어 있으면 `Lv.n`. 저장할 때 개수를 맞춰 두었다.
fn level_names_of(settings: &Map<String, Value>) -> Vec<String> {
    settings
        .get("level_names")
        .and_then(Value::as_str)
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn level_label(level: i64, names: &[String]) -> String {
    usize::try_from(level - 1)
        .ok()
        .and_then(|i| names.get(i))
        .cloned()
        .unwrap_or_else(|| format!("Lv.{level}"))
}

/// 1 부터 시작해 기준을 넘을 때마다 한 단계씩.
fn level_for(balance: i64, levels: &[i64]) -> i64 {
    1 + levels.iter().filter(|t| balance >= **t).count() as i64
}

#[async_trait]
impl Module for PointModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 20261105100001,
            name: "point",
            up: include_str!("../migrations/20261105100001_point.up.sql"),
            down: include_str!("../migrations/20261105100001_point.down.sql"),
        }]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/ranking", get(ranking_page))
                .layer(Extension(Arc::new(self.clone()))),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "mod/point/ranking.html",
                include_str!("../templates/ranking.html"),
            ),
            (
                "mod/point/point_ranking.html",
                include_str!("../templates/point_ranking.html"),
            ),
        ]
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "balance",
                description: "회원의 포인트와 레벨",
                input_schema: schema_of::<BalanceInput>(),
                permission: None,
            },
            ToolDef {
                name: "ranking",
                description: "포인트 순위",
                input_schema: schema_of::<RankingInput>(),
                permission: None,
            },
            ToolDef {
                name: "history",
                description: "회원의 포인트 내역",
                input_schema: schema_of::<HistoryInput>(),
                permission: None,
            },
            ToolDef {
                name: "adjust",
                description: "포인트를 손으로 더하거나 뺀다",
                input_schema: schema_of::<AdjustInput>(),
                permission: Some(Permission::MemberManage),
            },
        ]
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        match tool {
            "balance" => self.tool_balance(ctx, actor, parse_input(input)?).await,
            "history" => self.tool_history(ctx, actor, parse_input(input)?).await,
            "adjust" => self.tool_adjust(ctx, actor, parse_input(input)?).await,
            "ranking" => {
                let i: RankingInput = parse_input(input)?;
                let rows = self.ranking(ctx, i.limit.unwrap_or(10)).await?;
                Ok(json!({ "rows": rows }))
            }
            other => Err(module_error(
                Code::NotFound,
                format!("포인트 모듈에 없는 툴입니다: {other}"),
            )),
        }
    }

    async fn on_event(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        match ev.action.as_str() {
            "post.write" | "post.publish" => self.on_post(ctx, ev).await,
            "comment.create" | "comment.approve" => self.on_comment(ctx, ev).await,
            "auth.login" => self.on_login(ctx, ev).await,
            "post.delete" => match &ev.target_id {
                Some(id) => self.undo(&format!("post:{id}"), "글 삭제").await,
                None => Ok(()),
            },
            "comment.delete" => match &ev.target_id {
                Some(id) => self.undo(&format!("comment:{id}"), "댓글 삭제").await,
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }

    fn settings_fields(&self) -> Vec<FieldDef> {
        let number_field = |key: &str, label: &str, help: &str| FieldDef {
            key: key.to_owned(),
            label: label.to_owned(),
            kind: "number".to_owned(),
            required: true,
            options: Vec::new(),
            help: Some(help.to_owned()),
        };
        vec![
            number_field("post", "글", "글 하나를 발행할 때 주는 점수"),
            number_field("comment", "댓글", "댓글 하나에 주는 점수"),
            number_field("login", "로그인", "하루 첫 로그인에 주는 점수"),
            FieldDef {
                key: "levels".to_owned(),
                label: "레벨 기준".to_owned(),
                kind: "text".to_owned(),
                required: true,
                options: Vec::new(),
                help: Some("쉼표로 나눈 점수. 넘을 때마다 레벨이 하나 오릅니다".to_owned()),
            },
            FieldDef {
                key: "level_names".to_owned(),
                label: "레벨 이름".to_owned(),
                kind: "text".to_owned(),
                required: false,
                options: Vec::new(),
                help: Some(
                    "쉼표로 나눈 이름. 레벨 기준보다 하나 많게 적습니다. 비우면 Lv.1, Lv.2 …"
                        .to_owned(),
                ),
            },
        ]
    }

    fn default_settings(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("post".into(), json!(10));
        m.insert("comment".into(), json!(2));
        m.insert("login".into(), json!(1));
        m.insert("levels".into(), json!(DEFAULT_LEVELS));
        m.insert("level_names".into(), json!(""));
        m
    }

    fn check_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        for key in ["post", "comment", "login"] {
            if !settings.get(key).is_some_and(Value::is_i64) || number(settings, key) < 0 {
                return Err(module_error(
                    Code::Validation,
                    format!("{key} 는 0 이상의 정수여야 합니다."),
                ));
            }
        }
        let text = settings.get("levels").and_then(Value::as_str).unwrap_or("");
        let parsed: Option<Vec<i64>> = text.split(',').map(|s| s.trim().parse().ok()).collect();
        let levels = match parsed {
            Some(v) if v.windows(2).all(|w| w[0] < w[1]) && v.iter().all(|n| *n > 0) => v,
            _ => {
                return Err(module_error(
                    Code::Validation,
                    "레벨 기준은 커지는 양의 정수를 쉼표로 나눠 적으세요. 예: 100,500,2000",
                ));
            }
        };
        let names = level_names_of(settings);
        if !names.is_empty() && names.len() != levels.len() + 1 {
            return Err(module_error(
                Code::Validation,
                format!(
                    "레벨 이름은 {}개 적으세요(레벨 기준 {}개 + 1). 비우면 Lv.n 으로 보입니다.",
                    levels.len() + 1,
                    levels.len()
                ),
            ));
        }
        if names.iter().any(|n| n.chars().count() > MAX_LEVEL_NAME) {
            return Err(module_error(
                Code::Validation,
                format!("레벨 이름은 {MAX_LEVEL_NAME}자 이하로 적으세요."),
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
        if slot != MEMBER_BADGE || view.member_ids.is_empty() {
            return Ok(None);
        }
        let settings = settings_of(ctx, self).await?;
        let levels = levels_of(&settings);
        let names = level_names_of(&settings);
        let balances = self.balances_of(view.member_ids).await?;
        let by_member: Map<String, Value> = view
            .member_ids
            .iter()
            .map(|id| {
                let level = level_for(balances.get(id).copied().unwrap_or(0), &levels);
                (
                    id.clone(),
                    json!({ "label": level_label(level, &names), "title": format!("레벨 {level}") }),
                )
            })
            .collect();
        Ok(Some(SlotContent {
            template: "",
            data: json!({ "by_member": by_member }),
        }))
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        vec![WidgetType {
            id: WIDGET,
            name: "포인트 순위",
            description: "포인트가 많은 회원",
            config_schema: schema_of::<RankingWidget>(),
        }]
    }

    async fn check_widget(&self, _ctx: &Ctx, _kind: &str, config: Value) -> Result<Value> {
        let c: RankingWidget = serde_json::from_value(config)
            .map_err(|e| module_error(Code::Validation, format!("포인트 순위 위젯 설정: {e}")))?;
        if !(1..=20).contains(&c.limit) {
            return Err(module_error(
                Code::Validation,
                "포인트 순위 위젯의 limit 은 1~20 입니다.",
            ));
        }
        serde_json::to_value(c).map_err(Error::internal)
    }

    async fn widget_view(&self, ctx: &Ctx, _kind: &str, config: &Value) -> Result<Value> {
        let c: RankingWidget = serde_json::from_value(config.clone()).map_err(Error::internal)?;
        let rows = self.ranking(ctx, c.limit).await?;
        Ok(json!({ "rows": rows, "more_url": "/m/point/ranking" }))
    }
}

async fn ranking_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PointModule>>,
) -> Response {
    match module.ranking(&state.ctx, MAX_RANKING).await {
        Ok(rows) => {
            state
                .host
                .page(
                    "mod/point/ranking.html",
                    "포인트 순위",
                    json!({ "rows": rows }),
                )
                .await
        }
        Err(e) => state.host.error(&e).await,
    }
}
