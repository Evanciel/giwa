//! 게시판 서비스.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::fields::{self, FieldDef};
use crate::model::{BOARD_KINDS, BOARD_LAYOUTS, BOARD_ORDERS, Board, BoardPolicy};
use crate::services::required_text;
use crate::slug;
use crate::store::{Ctx, Tx};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewBoard {
    pub slug: String,
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub policy: Option<BoardPolicy>,
    #[serde(default)]
    pub sort_order: i64,
    /// 목록 형태. 비우면 종류의 기본값 (ADR-0054)
    #[serde(default)]
    pub layout: Option<String>,
    /// 글에 붙일 여분 칸 정의 (ADR-0057)
    #[serde(default)]
    pub field_schema: Option<Vec<FieldDef>>,
    /// 목록 순서 `latest`·`activity`. 비우면 종류의 기본값 (ADR-0058)
    #[serde(default)]
    pub list_order: Option<String>,
}

fn default_kind() -> String {
    "blog".into()
}

/// 결과와 함께 이번 호출로 새로 만들었는지 알려준다 (`--if-not-exists` 멱등성).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Created<T> {
    pub created: bool,
    #[serde(flatten)]
    pub item: T,
}

pub async fn create(ctx: &Ctx, actor: &Actor, input: NewBoard) -> Result<Board> {
    let res = create_inner(ctx, actor, input, false).await?;
    Ok(res.item)
}

/// 같은 슬러그가 있으면 그대로 돌려준다.
pub async fn ensure(ctx: &Ctx, actor: &Actor, input: NewBoard) -> Result<Created<Board>> {
    create_inner(ctx, actor, input, true).await
}

async fn create_inner(
    ctx: &Ctx,
    actor: &Actor,
    input: NewBoard,
    if_not_exists: bool,
) -> Result<Created<Board>> {
    actor.require(Permission::StructureEdit)?;
    let slug = slug::normalize(&input.slug)?;
    let name = required_text("name", &input.name, 100)?;
    check_kind(&input.kind)?;
    let layout = input.layout.unwrap_or_default();
    check_layout(&layout)?;
    let list_order = input.list_order.unwrap_or_default();
    check_order(&list_order)?;
    let field_schema = fields::check_schema(input.field_schema.unwrap_or_default())?;
    let policy = input.policy.unwrap_or_else(|| match input.kind.as_str() {
        "blog" => BoardPolicy::blog(),
        // 문의 게시판은 운영자가 답변글을 단다 (ADR-0056).
        "qna" => BoardPolicy {
            reply: "editor".into(),
            ..BoardPolicy::default()
        },
        _ => BoardPolicy::default(),
    });
    check_policy(&policy)?;

    let mut tx = Tx::begin(ctx).await?;
    if let Some(existing) = tx.board_by_slug(&slug).await? {
        if if_not_exists {
            return Ok(Created {
                created: false,
                item: existing,
            });
        }
        return Err(Error::new(Code::Conflict)
            .with_message("error.board_exists")
            .arg("slug", slug.clone())
            .next(format!("maru board show {slug}")));
    }
    let now = clock::now();
    let board = Board {
        id: clock::new_id(),
        slug,
        name,
        kind: input.kind,
        layout,
        list_order,
        policy,
        sort_order: input.sort_order,
        field_schema,
        created_at: now.clone(),
        updated_at: now,
    };
    tx.insert_board(&board).await?;
    tx.audit(
        actor,
        "board.create",
        Target::board(&board.id),
        audit::diff(None, Some(&board)),
    )
    .await?;
    tx.commit().await?;
    Ok(Created {
        created: true,
        item: board,
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub policy: Option<BoardPolicy>,
    #[serde(default)]
    pub sort_order: Option<i64>,
    /// 빈 문자열이면 종류의 기본값으로 되돌린다
    #[serde(default)]
    pub layout: Option<String>,
    /// 여분 칸 정의를 통째로 바꾼다. 빈 배열이면 모두 뺀다. 이미 쓴 글의 값은 그대로 둔다 (ADR-0057)
    #[serde(default)]
    pub field_schema: Option<Vec<FieldDef>>,
    /// 목록 순서. 빈 문자열이면 종류의 기본값으로 되돌린다 (ADR-0058)
    #[serde(default)]
    pub list_order: Option<String>,
}

pub async fn update(ctx: &Ctx, actor: &Actor, slug: &str, patch: BoardPatch) -> Result<Board> {
    actor.require(Permission::StructureEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx
        .board_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    let mut after = before.clone();
    if let Some(n) = &patch.name {
        after.name = required_text("name", n, 100)?;
    }
    if let Some(k) = patch.kind {
        check_kind(&k)?;
        after.kind = k;
    }
    if let Some(p) = patch.policy {
        check_policy(&p)?;
        after.policy = p;
    }
    if let Some(o) = patch.sort_order {
        after.sort_order = o;
    }
    if let Some(l) = patch.layout {
        check_layout(&l)?;
        after.layout = l;
    }
    if let Some(f) = patch.field_schema {
        after.field_schema = fields::check_schema(f)?;
    }
    if let Some(o) = patch.list_order {
        check_order(&o)?;
        after.list_order = o;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_board(&after).await?;
    tx.audit(
        actor,
        "board.update",
        Target::board(&after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 글이 하나도 없는 게시판만 지운다. 삭제된 글(`deleted`)도 글로 센다.
pub async fn delete(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Board> {
    actor.require(Permission::StructureEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let board = tx
        .board_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    let n = tx.count_board_posts(&board.id).await?;
    if n > 0 {
        return Err(Error::new(Code::Conflict)
            .with_message("error.board_not_empty")
            .arg("slug", slug)
            .arg("count", n.to_string())
            .next(format!("maru post list --board {slug}")));
    }
    tx.delete_board(&board.id).await?;
    tx.audit(
        actor,
        "board.delete",
        Target::board(&board.id),
        audit::diff(Some(&board), None),
    )
    .await?;
    tx.commit().await?;
    Ok(board)
}

/// 목록 권한이 있으면 게시판 하나를 본다.
pub async fn show(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Board> {
    let board = ctx
        .store
        .board_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    actor
        .require_rank(Permission::BoardList, &board.policy.list)
        .map_err(|_| not_found(slug))?;
    Ok(board)
}

fn check_kind(kind: &str) -> Result<()> {
    if BOARD_KINDS.contains(&kind) {
        return Ok(());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.board_kind")
        .arg("value", kind)
        .arg("allowed", BOARD_KINDS.join(", ")))
}

fn check_layout(layout: &str) -> Result<()> {
    if layout.is_empty() || BOARD_LAYOUTS.contains(&layout) {
        return Ok(());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.board_layout")
        .arg("value", layout)
        .arg("allowed", BOARD_LAYOUTS.join(", ")))
}

fn check_order(order: &str) -> Result<()> {
    if order.is_empty() || BOARD_ORDERS.contains(&order) {
        return Ok(());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.board_order")
        .arg("value", order)
        .arg("allowed", BOARD_ORDERS.join(", ")))
}

fn check_policy(policy: &BoardPolicy) -> Result<()> {
    for role in [&policy.list, &policy.read, &policy.write, &policy.moderate] {
        if crate::actor::rank::of_role(role).is_none() {
            return Err(Error::new(Code::Validation)
                .with_message("error.board_policy_role")
                .arg("value", role.clone()));
        }
    }
    // 파일 올리기는 회원 이상만. 손님에게 디스크를 열어 주지 않는다 (ADR-0065).
    if policy.upload != "none"
        && crate::actor::rank::of_role(&policy.upload)
            .is_none_or(|r| r < crate::actor::rank::MEMBER)
    {
        return Err(Error::new(Code::Validation)
            .with_message("error.board_upload_role")
            .arg("value", policy.upload.clone()));
    }
    // 댓글·답변은 `none`(받지 않음)도 된다 (ADR-0052·0056).
    for role in [&policy.comment, &policy.reply] {
        if role != "none" && crate::actor::rank::of_role(role).is_none() {
            return Err(Error::new(Code::Validation)
                .with_message("error.board_policy_role")
                .arg("value", role.clone()));
        }
    }
    Ok(())
}

/// 목록을 볼 수 있는 게시판.
pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<Board>> {
    let boards = ctx.store.list_boards().await?;
    Ok(boards
        .into_iter()
        .filter(|b| {
            actor
                .require_rank(Permission::BoardList, &b.policy.list)
                .is_ok()
        })
        .collect())
}

/// 읽을 수 있는 게시판만 찾는다. 읽을 수 없으면 존재를 드러내지 않고 `NOT_FOUND`.
pub async fn readable(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Board> {
    let board = ctx
        .store
        .board_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    if !actor.has(Permission::PostRead)
        || actor
            .require_rank(Permission::PostRead, &board.policy.read)
            .is_err()
    {
        return Err(not_found(slug));
    }
    Ok(board)
}

pub(crate) fn not_found(slug: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.board_not_found")
        .arg("slug", slug)
        .next("maru board list")
}
