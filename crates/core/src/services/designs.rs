//! 디자인 모음 (ADR-0158). 사이트 종류 프리셋의 첫 화면 본문 블록과 색·모양을 「디자인」 하나로 보여 준다.
//!
//! 프리셋 파일은 테마 크레이트에 있어(의존 방향) 부르는 쪽이 프리셋 목록을 넘긴다. 고른 디자인을 입히는 일은
//! 따로 두지 않는다. 화면 편집기는 초안에 싣고 발행 묶음으로, 에이전트는 `widget.place`·`tokens.set` 연산으로 한다.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use super::presets::{Op, Preset, theme_tokens};
use crate::actor::{Actor, Permission};
use crate::error::{Code, Error, Result};

/// 디자인 하나로 칠 최소 블록 수. 블록이 하나뿐인 프리셋(글 목록 등)은 화면 디자인이라 하기 어렵다.
const MIN_BLOCKS: usize = 2;

/// 목록에 보일 디자인 요약.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DesignInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    /// 색과 모양 설명
    pub look: String,
    /// 카드 견본 색: 바탕·글자·주 색·면
    pub swatch: Value,
    /// 첫 화면 본문 블록 수
    pub blocks: usize,
}

/// 첫 화면 본문 블록 하나. `widget.place` 에 `region: main` 으로 그대로 쓴다.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DesignBlock {
    pub kind: String,
    pub title: String,
    pub config: Value,
}

/// 디자인 전체: 블록과 입힐 토큰 패치(`tokens.set` 에 그대로).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Design {
    #[serde(flatten)]
    pub info: DesignInfo,
    pub tokens: Option<Value>,
    pub items: Vec<DesignBlock>,
}

/// 첫 화면 본문 블록이 둘 이상인 프리셋만 디자인이 된다. 권한을 보지 않는다(편집기 시작 묶음이 쓴다).
pub fn from_preset(preset: &Preset) -> Option<Design> {
    let items: Vec<DesignBlock> = preset
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::WidgetsEnsure { items } => Some(items),
            _ => None,
        })
        .flatten()
        .filter(|w| w.region == "main" && w.scope == "home")
        .map(|w| DesignBlock {
            kind: w.kind.clone(),
            title: w.title.clone(),
            config: w.config.clone(),
        })
        .collect();
    if items.len() < MIN_BLOCKS {
        return None;
    }
    let tokens = theme_tokens(preset);
    let color = tokens.as_ref().map_or(Value::Null, |t| t["color"].clone());
    let swatch = json!({
        "bg": color["bg"], "fg": color["fg"], "primary": color["primary"], "surface": color["surface"],
    });
    Some(Design {
        info: DesignInfo {
            id: preset.id.clone(),
            name: preset.name.clone(),
            description: preset.description.clone(),
            look: preset.theme_note.clone(),
            swatch,
            blocks: items.len(),
        },
        tokens,
        items,
    })
}

/// 디자인 목록. 사이트를 볼 수 있으면 누구나 본다.
pub fn list(actor: &Actor, presets: &[Preset]) -> Result<Vec<DesignInfo>> {
    actor.require(Permission::SiteView)?;
    Ok(presets
        .iter()
        .filter_map(from_preset)
        .map(|d| d.info)
        .collect())
}

/// 디자인 하나(블록·토큰).
pub fn show(actor: &Actor, preset: &Preset) -> Result<Design> {
    actor.require(Permission::SiteView)?;
    from_preset(preset).ok_or_else(|| {
        Error::new(Code::Validation)
            .with_message("error.usage_detail")
            .arg(
                "detail",
                format!("{} 에는 첫 화면 디자인이 없습니다.", preset.id),
            )
            .next("maru design gallery")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(ops: Value) -> Preset {
        serde_json::from_value(json!({ "id": "t", "name": "시험", "ops": ops })).unwrap()
    }

    #[test]
    fn only_presets_with_home_blocks_are_designs() {
        let one = preset(json!([
            { "op": "widgets.ensure", "items": [{ "kind": "hero", "region": "main", "scope": "home" }] }
        ]));
        assert!(from_preset(&one).is_none());
        let two = preset(json!([
            { "op": "tokens.ensure", "tokens": { "color": { "primary": "#9a3f18" } } },
            { "op": "widgets.ensure", "items": [
                { "kind": "hero", "region": "main", "scope": "home", "config": { "title": "어서 오세요" } },
                { "kind": "cta", "region": "main", "scope": "home" },
                { "kind": "recent_posts", "region": "sidebar", "scope": "all" }
            ] }
        ]));
        let d = from_preset(&two).unwrap();
        assert_eq!(d.info.blocks, 2);
        assert_eq!(d.items[0].config["title"], "어서 오세요");
        assert_eq!(d.info.swatch["primary"], "#9a3f18");
        // 테마 패치는 정하지 않은 색을 기본값으로 채운다.
        assert!(d.tokens.unwrap()["color"]["bg"].is_string());
        assert!(from_preset(&one).is_none() && from_preset(&two).is_some());
    }
}
