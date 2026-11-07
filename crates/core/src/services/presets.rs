//! 사이트 프리셋 (SPEC §8.6). 프리셋은 `[[ops]]` 목록이고 모든 op 는 `ensure` 의미라
//! 여러 번 적용해도 결과가 같다. 이미 사용자가 고친 것(메뉴 등)은 덮어쓰지 않는다.
//! Phase 3 에서 이 op 들이 changeset op 로 바뀐다 (ADR-0023).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::Actor;
use crate::error::Error;
use crate::error::Result;
use crate::model::{MenuItem, PageStatus, SiteSettings};
use crate::services::{boards, design, menus, pages, settings, widgets};
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
        /// 비우면 `default`
        #[serde(default)]
        layout: String,
        /// 위 페이지 주소. 앞선 연산에서 만들어 둔다 (ADR-0128)
        #[serde(default)]
        parent: String,
        #[serde(default)]
        sort_order: i64,
    },
    /// 모듈이 꺼져 있으면 켠다 (ADR-0127).
    #[serde(rename = "module.ensure")]
    ModuleEnsure { id: String },
    /// 메뉴가 비어 있을 때만 채운다.
    #[serde(rename = "menu.ensure")]
    MenuEnsure {
        location: String,
        items: Vec<MenuItem>,
    },
    /// 토큰이 기본값일 때만 덮어쓴다 (부분 JSON).
    #[serde(rename = "tokens.ensure")]
    TokensEnsure { tokens: Value },
    /// 사이트 설정 중 아직 기본값인 칸만 바꾼다. 값은 `SettingsPatch` 의 칸 (ADR-0081).
    #[serde(rename = "settings.ensure")]
    SettingsEnsure {
        values: serde_json::Map<String, Value>,
    },
    /// 놓을 위젯들의 영역·범위가 모두 비어 있을 때만 한꺼번에 놓는다 (ADR-0081).
    #[serde(rename = "widgets.ensure")]
    WidgetsEnsure { items: Vec<widgets::NewWidget> },
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

pub(crate) async fn apply_op(ctx: &Ctx, actor: &Actor, op: &Op) -> Result<OpResult> {
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
            layout,
            parent,
            sort_order,
        } => {
            let res = pages::ensure(
                ctx,
                actor,
                pages::NewPage {
                    slug: slug.clone(),
                    title: title.clone(),
                    body_md: body_md.clone(),
                    layout: if layout.is_empty() {
                        "default".into()
                    } else {
                        layout.clone()
                    },
                    sort_order: *sort_order,
                    parent: (!parent.is_empty()).then(|| parent.clone()),
                },
            )
            .await?;
            if res.created && *publish && res.item.status == PageStatus::Draft {
                pages::publish(ctx, actor, &res.item.slug).await?;
            }
            result("page.ensure", &res.item.slug, res.created)
        }
        Op::ModuleEnsure { id } => {
            let was_on = ctx.extensions.set_module(ctx, actor, id, true).await?;
            result("module.ensure", id, !was_on)
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
        Op::SettingsEnsure { values } => {
            let now = serde_json::to_value(settings::site(ctx).await?).map_err(Error::internal)?;
            let base = serde_json::to_value(SiteSettings::default()).map_err(Error::internal)?;
            let todo: serde_json::Map<String, Value> = values
                .iter()
                .filter(|(k, v)| {
                    now.get(k.as_str()) == base.get(k.as_str()) && now.get(k.as_str()) != Some(v)
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            // 모르는 칸·틀린 모양은 SettingsPatch(deny_unknown_fields)가 걸러 낸다.
            serde_json::from_value::<settings::SettingsPatch>(Value::Object(values.clone()))
                .map_err(|e| {
                    Error::new(crate::error::Code::Validation)
                        .with_message("error.preset_invalid")
                        .arg("id", "settings.ensure")
                        .arg("detail", e.to_string())
                })?;
            if !todo.is_empty() {
                let patch: settings::SettingsPatch =
                    serde_json::from_value(Value::Object(todo.clone())).map_err(Error::internal)?;
                settings::update(ctx, actor, patch).await?;
            }
            let keys: Vec<&str> = values.keys().map(String::as_str).collect();
            result("settings.ensure", &keys.join(","), !todo.is_empty())
        }
        Op::WidgetsEnsure { items } => {
            let placed = widgets::list(ctx, actor).await?;
            let empty = items.iter().all(|i| {
                !placed
                    .iter()
                    .any(|w| w.region == i.region && w.scope == i.scope)
            });
            if empty {
                for item in items {
                    widgets::place(ctx, actor, item.clone()).await?;
                }
            }
            result(
                "widgets.ensure",
                &format!("{}개", items.len()),
                empty && !items.is_empty(),
            )
        }
    })
}
