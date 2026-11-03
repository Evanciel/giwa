//! 사이트 프리셋 (SPEC §8.6). 프리셋은 `[[ops]]` 목록이고 모든 op 는 `ensure` 의미라
//! 여러 번 적용해도 결과가 같다. 이미 사용자가 고친 것(메뉴 등)은 덮어쓰지 않는다.
//! Phase 3 에서 이 op 들이 changeset op 로 바뀐다 (ADR-0023).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::Actor;
use crate::error::Result;
use crate::model::{MenuItem, PageStatus};
use crate::services::{boards, design, menus, pages};
use crate::store::Ctx;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op")]
pub enum Op {
    /// 게시판이 없으면 만든다.
    #[serde(rename = "board.ensure")]
    BoardEnsure(Box<boards::NewBoard>),
    /// 페이지가 없으면 만든다. `publish` 면 새로 만들 때만 공개한다.
    #[serde(rename = "page.ensure")]
    PageEnsure {
        slug: String,
        title: String,
        #[serde(default)]
        body_md: String,
        #[serde(default)]
        publish: bool,
    },
    /// 메뉴가 비어 있을 때만 채운다.
    #[serde(rename = "menu.ensure")]
    MenuEnsure {
        location: String,
        items: Vec<MenuItem>,
    },
    /// 토큰이 기본값일 때만 덮어쓴다 (부분 JSON).
    #[serde(rename = "tokens.ensure")]
    TokensEnsure { tokens: Value },
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OpResult {
    pub op: String,
    pub target: String,
    pub changed: bool,
}

pub async fn apply(ctx: &Ctx, actor: &Actor, preset: &Preset) -> Result<Vec<OpResult>> {
    let mut out = Vec::with_capacity(preset.ops.len());
    for op in &preset.ops {
        out.push(apply_op(ctx, actor, op).await?);
    }
    Ok(out)
}

async fn apply_op(ctx: &Ctx, actor: &Actor, op: &Op) -> Result<OpResult> {
    let result = |op: &str, target: &str, changed: bool| OpResult {
        op: op.to_owned(),
        target: target.to_owned(),
        changed,
    };
    Ok(match op {
        Op::BoardEnsure(input) => {
            let res = boards::ensure(ctx, actor, (**input).clone()).await?;
            result("board.ensure", &res.item.slug, res.created)
        }
        Op::PageEnsure {
            slug,
            title,
            body_md,
            publish,
        } => {
            let res = pages::ensure(
                ctx,
                actor,
                pages::NewPage {
                    slug: slug.clone(),
                    title: title.clone(),
                    body_md: body_md.clone(),
                    layout: "default".into(),
                    sort_order: 0,
                },
            )
            .await?;
            if res.created && *publish && res.item.status == PageStatus::Draft {
                pages::publish(ctx, actor, &res.item.slug).await?;
            }
            result("page.ensure", &res.item.slug, res.created)
        }
        Op::MenuEnsure { location, items } => {
            let empty = menus::get(ctx, location).await?.is_empty();
            if empty && !items.is_empty() {
                menus::set(ctx, actor, location, items.clone()).await?;
            }
            result("menu.ensure", location, empty && !items.is_empty())
        }
        Op::TokensEnsure { tokens } => {
            let is_default = design::tokens(ctx).await? == crate::design::Tokens::default();
            if is_default {
                design::update(ctx, actor, tokens.clone()).await?;
            }
            let changed =
                is_default && design::tokens(ctx).await? != crate::design::Tokens::default();
            result("tokens.ensure", "design.tokens", changed)
        }
    })
}
