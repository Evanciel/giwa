//! 게시판 분류 (GAP C1). 분류는 게시판마다 따로 있고, 글은 분류를 하나 가질 수 있다.
//!
//! 만들기는 주소(slug) 기준으로 멱등이다: 같은 주소가 있으면 그것을 돌려준다.
//! 지우면 그 분류의 글은 분류가 비워진다(글은 남는다).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{Board, Category};
use crate::services::{boards, required_text};
use crate::slug;
use crate::store::{Ctx, Tx, WriteTx};

const MAX_NAME: usize = 40;
const MAX_PER_BOARD: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCategory {
    /// 게시판 슬러그
    pub board: String,
    pub name: String,
    /// 비우면 이름에서 만든다
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CategoryPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    /// 작을수록 앞
    #[serde(default)]
    pub sort_order: Option<i64>,
}

/// 게시판의 분류 (정렬 순서대로). 게시판 목록을 볼 수 있으면 본다.
pub async fn list(ctx: &Ctx, actor: &Actor, board: &str) -> Result<Vec<Category>> {
    let board = boards::show(ctx, actor, board).await?;
    ctx.store.categories_of_board(&board.id).await
}

/// 분류를 만든다. 같은 주소가 이미 있으면 그것을 그대로 돌려준다.
pub async fn create(ctx: &Ctx, actor: &Actor, input: NewCategory) -> Result<Category> {
    actor.require(Permission::StructureEdit)?;
    let name = required_text("name", &input.name, MAX_NAME)?;
    let slug = slug_of(input.slug.as_deref(), &name)?;
    let mut tx = Tx::begin(ctx).await?;
    let board = board_for_write(&mut *tx, &input.board).await?;
    if let Some(found) = tx.category_by_slug(&board.id, &slug).await? {
        return Ok(found);
    }
    let existing = tx.categories_of_board(&board.id).await?;
    if existing.len() >= MAX_PER_BOARD {
        return Err(Error::new(Code::Validation)
            .with_message("error.category_too_many")
            .arg("max", MAX_PER_BOARD.to_string()));
    }
    let now = clock::now();
    let category = Category {
        id: clock::new_id(),
        board_id: board.id.clone(),
        name,
        slug,
        sort_order: existing.iter().map(|c| c.sort_order + 1).max().unwrap_or(0),
        created_at: now.clone(),
        updated_at: now,
    };
    tx.insert_category(&category).await?;
    tx.audit(
        actor,
        "category.create",
        Target::new("category", &category.id),
        audit::diff(None, Some(&category)),
    )
    .await?;
    tx.commit().await?;
    Ok(category)
}

/// 이름·주소·순서를 바꾼다. 바뀐 것이 없으면 그대로 돌려준다.
pub async fn update(
    ctx: &Ctx,
    actor: &Actor,
    board: &str,
    slug: &str,
    patch: CategoryPatch,
) -> Result<Category> {
    actor.require(Permission::StructureEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let board = board_for_write(&mut *tx, board).await?;
    let before = find(&mut *tx, &board, slug).await?;
    let mut after = before.clone();
    if let Some(n) = &patch.name {
        after.name = required_text("name", n, MAX_NAME)?;
    }
    if let Some(s) = &patch.slug {
        after.slug = slug_of(Some(s), &after.name)?;
        if after.slug != before.slug && tx.category_by_slug(&board.id, &after.slug).await?.is_some()
        {
            return Err(Error::new(Code::Conflict)
                .with_message("error.category_exists")
                .arg("board", board.slug.clone())
                .arg("slug", after.slug.clone()));
        }
    }
    if let Some(o) = patch.sort_order {
        after.sort_order = o;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_category(&after).await?;
    tx.audit(
        actor,
        "category.update",
        Target::new("category", &after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 분류를 지운다. 그 분류의 글은 분류가 비워진다.
pub async fn delete(ctx: &Ctx, actor: &Actor, board: &str, slug: &str) -> Result<Category> {
    actor.require(Permission::StructureEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let board = board_for_write(&mut *tx, board).await?;
    let before = find(&mut *tx, &board, slug).await?;
    let cleared = tx.delete_category(&before.id).await?;
    let mut diff = audit::diff(Some(&before), None);
    if let Some(obj) = diff.as_object_mut() {
        obj.insert("posts_cleared".into(), cleared.into());
    }
    tx.audit(
        actor,
        "category.delete",
        Target::new("category", &before.id),
        diff,
    )
    .await?;
    tx.commit().await?;
    Ok(before)
}

/// 글에 붙일 분류를 찾는다. 빈 문자열이면 분류 없음.
pub(crate) async fn resolve(
    tx: &mut dyn WriteTx,
    board: &Board,
    slug: &str,
) -> Result<Option<Category>> {
    let slug = slug.trim();
    if slug.is_empty() {
        return Ok(None);
    }
    let normalized = slug::normalize(slug).map_err(|_| not_found(board, slug))?;
    tx.category_by_slug(&board.id, &normalized)
        .await?
        .map(Some)
        .ok_or_else(|| not_found(board, slug))
}

fn slug_of(given: Option<&str>, name: &str) -> Result<String> {
    match given.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => slug::normalize(s),
        None => Ok(slug::from_title(name, "category")),
    }
}

async fn board_for_write(tx: &mut dyn WriteTx, slug: &str) -> Result<Board> {
    tx.board_by_slug(slug)
        .await?
        .ok_or_else(|| boards::not_found(slug))
}

async fn find(tx: &mut dyn WriteTx, board: &Board, slug: &str) -> Result<Category> {
    let normalized = slug::normalize(slug).map_err(|_| not_found(board, slug))?;
    tx.category_by_slug(&board.id, &normalized)
        .await?
        .ok_or_else(|| not_found(board, slug))
}

fn not_found(board: &Board, slug: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.category_not_found")
        .arg("board", board.slug.clone())
        .arg("slug", slug)
        .next(format!("maru category list --board {}", board.slug))
}
