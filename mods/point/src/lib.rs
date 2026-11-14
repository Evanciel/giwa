//! 포인트 모듈 (GAP M13, SPEC Phase 2 샘플, ADR-0067).
//!
//! 글·댓글·로그인(하루 한 번)에 적립하고, 글·댓글이 지워지면 준 만큼 되돌린다. 모든 증감은 원장에 한 줄씩
//! 남고 `source` 가 유일해서 같은 이벤트가 다시 와도 두 번 적립되지 않는다.
//!
//! 출석체크(GAP Q10, ADR-0095): 회원이 하루 한 번 `/m/point/attend` 에서 출석하면 점수를 주고,
//! 연달아 출석한 날수를 세어 정해진 날수마다 보너스를 준다.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Extension, Router};
use giwa_core::actor::{Actor, Permission};
use giwa_core::audit::{AuditRecord, Target};
use giwa_core::fields::FieldDef;
use giwa_core::model::{AuditEvent, CommentStatus, Member, MemberStatus, PostStatus};
use giwa_core::services::widgets::WidgetType;
use giwa_core::store::ModuleMigration;
use giwa_core::urls;
use giwa_core::{Code, Ctx, Error, Result, clock};
use giwa_modules::service::settings_of;
use giwa_modules::{
    ACCOUNT_BAR, MEMBER_BADGE, Manifest, Module, ModuleState, SlotContent, SlotView, ToolDef,
    module_error, schema_of,
};
use giwa_storage::Db;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

static MANIFEST: Manifest = Manifest {
    id: "point",
    name: "포인트",
    version: env!("CARGO_PKG_VERSION"),
    description: "글·댓글·로그인·출석에 포인트를 주고 레벨과 순위를 보여 줍니다.",
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
/// 출석 화면 주소.
const ATTEND_PATH: &str = "/m/point/attend";
/// 이 날수만큼 연달아 출석할 때마다 연속 보너스를 준다 (ADR-0095).
const ATTEND_CYCLE: i64 = 7;
/// 출석 화면에 보이는 그날 출석한 사람 수 상한.
const MAX_ATTEND_LIST: i64 = 100;

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
                giwa_storage::sqlite::insert_audit(&mut tx, rec).await?;
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

    /// 오늘 출석한다 (GAP Q10, ADR-0095). 사용 중인 회원만, 하루 한 줄. 다시 불러도 안전하다 —
    /// 적립 열쇠 `attend:<회원>:<날>` 이 같아 두 번 주지 않는다.
    async fn attend(&self, ctx: &Ctx, actor: &Actor) -> Result<Value> {
        let Some(member_id) = actor.member_id() else {
            return Err(module_error(
                Code::Unauthorized,
                "출석은 회원으로 로그인해야 합니다.",
            ));
        };
        let member = find_member(ctx, member_id).await?;
        if member.status != MemberStatus::Active {
            return Err(module_error(
                Code::Forbidden,
                "사용 중인 회원만 출석할 수 있습니다.",
            ));
        }
        let at = clock::now();
        let day = local_day(&at);
        let prev = prev_day(&day).unwrap_or_default();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let before = sqlx::query_scalar!(
            "SELECT streak FROM mod_point_attend WHERE member_id = ? AND day = ?",
            member.id,
            prev
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let streak = before.unwrap_or(0) + 1;
        let inserted = sqlx::query!(
            "INSERT OR IGNORE INTO mod_point_attend (member_id, day, streak, at) VALUES (?, ?, ?, ?)",
            member.id,
            day,
            streak,
            at
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected()
            == 1;
        if inserted {
            let rec = AuditRecord::new(
                actor,
                "point.attend",
                Target::new("member", &member.id),
                json!({ "day": day, "streak": streak }),
            );
            giwa_storage::sqlite::insert_audit(&mut tx, &rec).await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        let row = sqlx::query!(
            "SELECT streak, at FROM mod_point_attend WHERE member_id = ? AND day = ?",
            member.id,
            day
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        // 적립은 출석 줄과 따로 한다. 그 사이에 끊겨도 다시 부르면 빠진 것만 채운다.
        let settings = settings_of(ctx, self).await?;
        let mut points = 0;
        let amount = number(&settings, "attend");
        let source = format!("attend:{}:{day}", member.id);
        if self
            .credit(&member.id, amount, "출석", &source, None)
            .await?
        {
            points += amount;
        }
        let bonus = number(&settings, "attend_bonus");
        if row.streak % ATTEND_CYCLE == 0 {
            let reason = format!("{}일 연속 출석", row.streak);
            let source = format!("attend-bonus:{}:{day}", member.id);
            if self
                .credit(&member.id, bonus, &reason, &source, None)
                .await?
            {
                points += bonus;
            }
        }
        let rank = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM mod_point_attend
             WHERE day = ? AND (at < ? OR (at = ? AND member_id <= ?))",
            day,
            row.at,
            row.at,
            member.id
        )
        .fetch_one(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(json!({
            "day": day,
            "streak": row.streak,
            "already": !inserted,
            "points": points,
            "rank": rank,
        }))
    }

    /// 그날 출석한 수와 먼저 온 순서의 회원들. 사용 중이 아닌 회원은 건너뛴다(순위 번호는 그대로).
    async fn day_list(&self, ctx: &Ctx, day: &str) -> Result<(i64, Vec<AttendRow>)> {
        let total = sqlx::query_scalar!("SELECT COUNT(*) FROM mod_point_attend WHERE day = ?", day)
            .fetch_one(self.db.read_pool())
            .await
            .map_err(Error::internal)?;
        let rows = sqlx::query!(
            "SELECT member_id, streak, at FROM mod_point_attend
             WHERE day = ? ORDER BY at, member_id LIMIT ?",
            day,
            MAX_ATTEND_LIST
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (i, r) in rows.into_iter().enumerate() {
            let Some(member) = ctx.store.member_by_id(&r.member_id).await? else {
                continue;
            };
            if member.status != MemberStatus::Active {
                continue;
            }
            out.push(AttendRow {
                rank: i as i64 + 1,
                member_id: r.member_id,
                name: member.display_name,
                streak: r.streak,
                time: local_time(&r.at),
            });
        }
        Ok((total, out))
    }

    /// 한 회원의 한 달(`YYYY-MM`) 출석: 날짜와 그날의 연속 일수.
    async fn month_days(&self, member_id: &str, month: &str) -> Result<Vec<(String, i64)>> {
        let pattern = format!("{month}-%");
        let rows = sqlx::query!(
            "SELECT day, streak FROM mod_point_attend
             WHERE member_id = ? AND day LIKE ? ORDER BY day",
            member_id,
            pattern
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|r| (r.day, r.streak)).collect())
    }

    /// (오늘 출석했나, 지금 이어지는 연속 일수). 오늘이나 어제 출석했으면 그 줄의 연속 일수, 아니면 0.
    async fn current_streak(&self, member_id: &str, today: &str) -> Result<(bool, i64)> {
        let last = sqlx::query!(
            "SELECT day, streak FROM mod_point_attend WHERE member_id = ? ORDER BY day DESC LIMIT 1",
            member_id
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(match last {
            Some(r) if r.day == today => (true, r.streak),
            Some(r) if Some(r.day.as_str()) == prev_day(today).as_deref() => (false, r.streak),
            _ => (false, 0),
        })
    }

    /// 출석 화면 데이터. 오늘 출석한 회원은 누구에게나 같고, 로그인한 회원에겐 내 달력이 더해진다.
    async fn attend_view(&self, ctx: &Ctx, actor: &Actor, month: Option<&str>) -> Result<Value> {
        let today = local_day(&clock::now());
        let Some((ty, tm, td)) = parse_day(&today) else {
            return Err(module_error(Code::Internal, "오늘 날짜를 읽지 못했습니다."));
        };
        // 모르는 달·앞으로 올 달은 이번 달로.
        let (y, m) = month
            .and_then(parse_month)
            .filter(|ym| *ym <= (ty, tm))
            .unwrap_or((ty, tm));
        let settings = settings_of(ctx, self).await?;
        let (count, rows) = self.day_list(ctx, &today).await?;
        let me = match actor.member_id() {
            Some(id) => {
                let (attended, streak) = self.current_streak(id, &today).await?;
                let days = self.month_days(id, &format!("{y:04}-{m:02}")).await?;
                let on: HashSet<u32> = days
                    .iter()
                    .filter_map(|(d, _)| d.get(8..10)?.parse().ok())
                    .collect();
                let today_in_month = ((y, m) == (ty, tm)).then_some(td);
                let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
                let next = ((y, m) < (ty, tm)).then(|| {
                    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                    format!("{ATTEND_PATH}?month={ny:04}-{nm:02}")
                });
                Some(json!({
                    "attended": attended,
                    "streak": streak,
                    "month_count": days.len(),
                    "calendar": {
                        "label": format!("{y}년 {m}월"),
                        "weeks": month_weeks(y, m, &on, today_in_month),
                        "prev_url": format!("{ATTEND_PATH}?month={py:04}-{pm:02}"),
                        "next_url": next,
                    },
                }))
            }
            None => None,
        };
        Ok(json!({
            "today_label": format!("{tm}월 {td}일"),
            "count": count,
            "rows": rows,
            "me": me,
            "attend": number(&settings, "attend"),
            "bonus": number(&settings, "attend_bonus"),
            "cycle": ATTEND_CYCLE,
            "login_url": urls::with_query("/login", &[("next", ATTEND_PATH)]),
            "noindex": true,
        }))
    }

    async fn tool_attendance(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        input: AttendanceInput,
    ) -> Result<Value> {
        let today = local_day(&clock::now());
        if let Some(key) = input.member.as_deref().filter(|k| !k.trim().is_empty()) {
            let member = find_member(ctx, key.trim()).await?;
            if actor.member_id() != Some(&member.id) {
                actor.require(Permission::MemberManage)?;
            }
            let month = match input.month {
                Some(m) if parse_month(&m).is_some() => m,
                Some(_) => {
                    return Err(module_error(
                        Code::Validation,
                        "month 는 YYYY-MM 으로 적으세요. 예: 2026-11",
                    ));
                }
                None => today.chars().take(7).collect(),
            };
            let (attended, streak) = self.current_streak(&member.id, &today).await?;
            let days: Vec<Value> = self
                .month_days(&member.id, &month)
                .await?
                .into_iter()
                .map(|(day, streak)| json!({ "day": day, "streak": streak }))
                .collect();
            return Ok(json!({
                "member_id": member.id,
                "month": month,
                "days": days,
                "attended_today": attended,
                "streak": streak,
            }));
        }
        let day = match input.day {
            Some(d) if parse_day(&d).is_some() => d,
            Some(_) => {
                return Err(module_error(
                    Code::Validation,
                    "day 는 있는 날짜를 YYYY-MM-DD 로 적으세요. 예: 2026-11-05",
                ));
            }
            None => today,
        };
        let (count, rows) = self.day_list(ctx, &day).await?;
        Ok(json!({ "day": day, "count": count, "rows": rows }))
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

#[derive(Debug, Clone, Serialize)]
struct AttendRow {
    rank: i64,
    member_id: String,
    name: String,
    streak: i64,
    /// 출석한 시각 `09:00` (한국 시간)
    time: String,
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

/// 오늘 출석 (로그인한 회원 본인).
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttendInput {}

/// 출석 기록.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttendanceInput {
    /// 그날 출석한 회원 (YYYY-MM-DD). 비우면 오늘
    #[serde(default)]
    day: Option<String>,
    /// 이 회원의 한 달 출석 (회원 ID 또는 이메일). 남의 기록은 회원 관리 권한이 있어야 본다
    #[serde(default)]
    member: Option<String>,
    /// member 와 함께: 볼 달 (YYYY-MM). 비우면 이번 달
    #[serde(default)]
    month: Option<String>,
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

/// 사이트 날짜 `2026-11-05`. 하루는 한국 시간으로 가른다(로그인 적립과 같다).
fn local_day(at: &str) -> String {
    clock::to_local_input(at, DAY_OFFSET_MINUTES)
        .chars()
        .take(10)
        .collect()
}

/// 사이트 시각 `09:00`.
fn local_time(at: &str) -> String {
    clock::to_local_input(at, DAY_OFFSET_MINUTES)
        .chars()
        .skip(11)
        .collect()
}

/// `YYYY-MM` 을 (연, 월)로. 모양이 다르면 없음.
fn parse_month(s: &str) -> Option<(i32, u32)> {
    let (y, m) = s.split_once('-')?;
    let digits = y.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit());
    if y.len() != 4 || m.len() != 2 || !digits {
        return None;
    }
    let (y, m) = (y.parse().ok()?, m.parse().ok()?);
    (1..=12).contains(&m).then_some((y, m))
}

/// `YYYY-MM-DD` 를 (연, 월, 일)로. 없는 날(2월 30일 등)은 없음.
fn parse_day(s: &str) -> Option<(i32, u32, u32)> {
    let (ym, d) = s.rsplit_once('-')?;
    let (y, m) = parse_month(ym)?;
    if d.len() != 2 || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let d = d.parse().ok()?;
    (1..=days_in_month(y, m)).contains(&d).then_some((y, m, d))
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// 전날 `YYYY-MM-DD`.
fn prev_day(s: &str) -> Option<String> {
    let (y, m, d) = parse_day(s)?;
    let (y, m, d) = match (m, d) {
        (1, 1) => (y - 1, 12, 31),
        (_, 1) => (y, m - 1, days_in_month(y, m - 1)),
        _ => (y, m, d - 1),
    };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// 요일, 0 = 일요일 (Sakamoto 공식).
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let t = (m as usize)
        .checked_sub(1)
        .and_then(|i| T.get(i))
        .copied()
        .unwrap_or(0);
    (y + y / 4 - y / 100 + y / 400 + t + d as i32).rem_euclid(7) as u32
}

/// 달력 칸: 일요일부터 한 주씩, 그달이 아닌 칸은 null.
fn month_weeks(y: i32, m: u32, on: &HashSet<u32>, today: Option<u32>) -> Vec<Vec<Value>> {
    let mut cells: Vec<Value> = (0..weekday(y, m, 1)).map(|_| Value::Null).collect();
    for d in 1..=days_in_month(y, m) {
        cells.push(json!({ "day": d, "on": on.contains(&d), "today": today == Some(d) }));
    }
    while !cells.len().is_multiple_of(7) {
        cells.push(Value::Null);
    }
    cells.chunks(7).map(<[Value]>::to_vec).collect()
}

#[async_trait]
impl Module for PointModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![
            ModuleMigration {
                version: 20261105100001,
                name: "point",
                up: include_str!("../migrations/20261105100001_point.up.sql"),
                down: include_str!("../migrations/20261105100001_point.down.sql"),
            },
            ModuleMigration {
                version: 20261106100001,
                name: "attend",
                up: include_str!("../migrations/20261106100001_attend.up.sql"),
                down: include_str!("../migrations/20261106100001_attend.down.sql"),
            },
        ]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/ranking", get(ranking_page))
                .route("/attend", get(attend_page).post(attend_form))
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
            (
                "mod/point/attend.html",
                include_str!("../templates/attend.html"),
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
                name: "attend",
                description: "오늘 출석한다 (로그인한 회원 본인, 하루 한 번)",
                input_schema: schema_of::<AttendInput>(),
                permission: None,
            },
            ToolDef {
                name: "attendance",
                description: "그날 출석한 회원, 또는 한 회원의 한 달 출석",
                input_schema: schema_of::<AttendanceInput>(),
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
            "attend" => {
                let _: AttendInput = parse_input(input)?;
                self.attend(ctx, actor).await
            }
            "attendance" => self.tool_attendance(ctx, actor, parse_input(input)?).await,
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
            list: false,
            filter: false,
            role: None,
        };
        vec![
            number_field("post", "글", "글 하나를 발행할 때 주는 점수"),
            number_field("comment", "댓글", "댓글 하나에 주는 점수"),
            number_field("login", "로그인", "하루 첫 로그인에 주는 점수"),
            number_field("attend", "출석", "하루 한 번 출석체크에 주는 점수"),
            number_field(
                "attend_bonus",
                "연속 출석 보너스",
                "7일 연달아 출석할 때마다 더 주는 점수",
            ),
            FieldDef {
                key: "levels".to_owned(),
                label: "레벨 기준".to_owned(),
                kind: "text".to_owned(),
                required: true,
                options: Vec::new(),
                help: Some("쉼표로 나눈 점수. 넘을 때마다 레벨이 하나 오릅니다".to_owned()),
                list: false,
                filter: false,
                role: None,
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
                list: false,
                filter: false,
                role: None,
            },
        ]
    }

    fn default_settings(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("post".into(), json!(10));
        m.insert("comment".into(), json!(2));
        m.insert("login".into(), json!(1));
        m.insert("attend".into(), json!(5));
        m.insert("attend_bonus".into(), json!(20));
        m.insert("levels".into(), json!(DEFAULT_LEVELS));
        m.insert("level_names".into(), json!(""));
        m
    }

    fn check_settings(&self, settings: &Map<String, Value>) -> Result<()> {
        for key in ["post", "comment", "login", "attend", "attend_bonus"] {
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
        if slot == ACCOUNT_BAR {
            // 이름 메뉴의 「출석체크」와 내 레벨·포인트 한 줄 (GAP Q14, ADR-0099)
            let Some(member) = view.viewer.member_id() else {
                return Ok(None);
            };
            let settings = settings_of(ctx, self).await?;
            let balance = self.balance_of(member).await?;
            let level = level_for(balance, &levels_of(&settings));
            return Ok(Some(SlotContent {
                template: "",
                data: json!({
                    "label": "출석체크",
                    "url": ATTEND_PATH,
                    "menu": true,
                    "note": format!("{} · {balance}점", level_label(level, &level_names_of(&settings))),
                }),
            }));
        }
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

#[derive(Debug, Deserialize)]
struct AttendQuery {
    #[serde(default)]
    month: Option<String>,
}

async fn attend_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PointModule>>,
    Query(q): Query<AttendQuery>,
    headers: HeaderMap,
) -> Response {
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    match module
        .attend_view(&state.ctx, &actor, q.month.as_deref())
        .await
    {
        Ok(data) => {
            let mut res = state
                .host
                .page("mod/point/attend.html", "출석체크", data)
                .await;
            // 사람마다 다른 화면(내 달력)이라 공유 캐시에 두지 않는다.
            res.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-cache"),
            );
            res
        }
        Err(e) => state.host.error(&e).await,
    }
}

async fn attend_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<PointModule>>,
    headers: HeaderMap,
) -> Response {
    if !state.host.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    if actor.member_id().is_none() {
        return Redirect::to(&urls::with_query("/login", &[("next", ATTEND_PATH)])).into_response();
    }
    match module.attend(&state.ctx, &actor).await {
        Ok(_) => Redirect::to(ATTEND_PATH).into_response(),
        Err(e) => state.host.error(&e).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_day("2026-11-05"), Some((2026, 11, 5)));
        assert_eq!(parse_day("2026-02-30"), None);
        assert_eq!(parse_day("2026-1-05"), None);
        assert_eq!(parse_day("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(prev_day("2026-03-01").as_deref(), Some("2026-02-28"));
        assert_eq!(prev_day("2024-03-01").as_deref(), Some("2024-02-29"));
        assert_eq!(prev_day("2026-01-01").as_deref(), Some("2025-12-31"));
        assert_eq!(prev_day("2026-11-05").as_deref(), Some("2026-11-04"));
        assert_eq!(weekday(2026, 11, 1), 0, "2026-11-01 은 일요일");
        assert_eq!(weekday(2026, 11, 5), 4, "목요일");
        assert_eq!(weekday(2024, 2, 29), 4, "목요일");
        assert_eq!(
            local_day("2026-11-04T15:30:00Z"),
            "2026-11-05",
            "한국 시간 자정 넘김"
        );
        assert_eq!(local_time("2026-11-04T15:30:00Z"), "00:30");
    }

    #[test]
    fn calendar_cells() {
        let on: HashSet<u32> = [1, 30].into_iter().collect();
        let weeks = month_weeks(2026, 11, &on, Some(5));
        assert_eq!(weeks.len(), 5);
        assert_eq!(weeks[0][0]["day"], 1);
        assert_eq!(weeks[0][0]["on"], true);
        assert_eq!(weeks[0][4]["today"], true);
        assert_eq!(weeks[4][1]["day"], 30);
        assert!(weeks[4][2].is_null());
    }
}
