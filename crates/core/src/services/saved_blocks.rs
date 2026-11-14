//! 내 블록 (ADR-0159). 화면 편집기에서 고친 블록을 이름을 붙여 저장하고 다른 화면에 다시 넣는다.
//!
//! 보관함은 방문자에게 보이지 않아 변경 묶음을 거치지 않는다(에이전트도 바로 저장한다). 넣는 일은 블록 놓기라
//! 지금처럼 발행 묶음(사람)이나 변경 묶음의 `widget.place`(에이전트)로 한다. 저장·지우기는 감사 기록에 남는다.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::SavedBlock;
use crate::store::{Ctx, Tx};

use super::widgets;

/// 보관함에 둘 수 있는 블록 수.
pub const MAX_BLOCKS: usize = 200;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSavedBlock {
    /// 보관함에 보일 이름 (40자)
    pub name: String,
    /// 블록 종류 (`giwa widget types`)
    pub kind: String,
    /// 놓을 자리. 비우면 `main`
    #[serde(default)]
    pub region: String,
    /// 블록 제목
    #[serde(default)]
    pub title: String,
    /// 블록 설정. 놓을 때와 같은 규칙으로 검사한다
    #[serde(default)]
    pub config: Value,
}

pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<SavedBlock>> {
    actor.require(Permission::DesignEdit)?;
    ctx.store.saved_blocks().await
}

pub async fn save(ctx: &Ctx, actor: &Actor, input: NewSavedBlock) -> Result<SavedBlock> {
    actor.require(Permission::DesignEdit)?;
    let name = super::required_text("name", &input.name, 40)?;
    let title = input.title.trim().to_owned();
    if title.chars().count() > 100 {
        return Err(Error::new(Code::Validation)
            .with_message("error.field_length")
            .arg("field", "title")
            .arg("max", "100"));
    }
    let region = match input.region.trim() {
        "" => "main",
        r => r,
    };
    let config = widgets::check_draft(ctx, &input.kind, region, input.config).await?;
    let kind = input.kind.trim().to_owned();
    let all = ctx.store.saved_blocks().await?;
    if let Some(same) = all.iter().find(|b| {
        b.name == name
            && b.kind == kind
            && b.region == region
            && b.title == title
            && b.config == config
    }) {
        return Ok(same.clone());
    }
    if all.len() >= MAX_BLOCKS {
        return Err(Error::new(Code::Validation)
            .with_message("error.saved_blocks_full")
            .arg("max", MAX_BLOCKS.to_string())
            .next("giwa widget saved list"));
    }
    let block = SavedBlock {
        id: clock::new_id(),
        name,
        kind,
        region: region.to_owned(),
        title,
        config,
        created_by: actor.member_id().cloned(),
        created_at: clock::now(),
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.insert_saved_block(&block).await?;
    tx.audit(
        actor,
        "block.save",
        Target::new("saved_block", &block.id),
        audit::diff(None, Some(&block)),
    )
    .await?;
    tx.commit().await?;
    Ok(block)
}

/// 지운다. 없었으면 `None`(여러 번 불러도 같다).
pub async fn delete(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Option<SavedBlock>> {
    actor.require(Permission::DesignEdit)?;
    let Some(block) = ctx.store.saved_block(id).await? else {
        return Ok(None);
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.delete_saved_block(&block.id).await?;
    tx.audit(
        actor,
        "block.delete",
        Target::new("saved_block", &block.id),
        audit::diff(Some(&block), None),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(block))
}
