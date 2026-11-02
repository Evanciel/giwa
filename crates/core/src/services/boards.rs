//! 게시판 서비스.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{BOARD_KINDS, Board, BoardPolicy};
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
    if !BOARD_KINDS.contains(&input.kind.as_str()) {
        return Err(Error::new(Code::Validation)
            .with_message("error.board_kind")
            .arg("value", input.kind)
            .arg("allowed", BOARD_KINDS.join(", ")));
    }
    let policy = input.policy.unwrap_or_else(|| {
        if input.kind == "blog" {
            BoardPolicy::blog()
        } else {
            BoardPolicy::default()
        }
    });
    for role in [&policy.list, &policy.read, &policy.write, &policy.moderate] {
        if crate::actor::rank::of_role(role).is_none() {
            return Err(Error::new(Code::Validation)
                .with_message("error.board_policy_role")
                .arg("value", role.clone()));
        }
    }

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
        policy,
        sort_order: input.sort_order,
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
