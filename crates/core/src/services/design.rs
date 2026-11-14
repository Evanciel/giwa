//! 디자인 토큰 저장. `site_settings` 의 `design.tokens` 키 하나에 전체 토큰을 둔다.
//! Phase 3 에서 changeset op(`tokens.set`)로 감싼다. 지금은 `design.edit` 권한으로 바로 반영한다.

use serde_json::{Value, json};

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::design::{Tokens, font_stack};
use crate::error::{Code, Error, Result};
use crate::store::{Ctx, Tx};

const KEY: &str = "design.tokens";

/// 저장된 토큰. 없거나 깨졌으면 기본값 (화면이 깨지지 않게).
pub async fn tokens(ctx: &Ctx) -> Result<Tokens> {
    Ok(from_pairs(ctx.store.settings().await?))
}

fn from_pairs(pairs: Vec<(String, Value)>) -> Tokens {
    pairs
        .into_iter()
        .find(|(k, _)| k == KEY)
        .and_then(|(_, v)| serde_json::from_value::<Tokens>(v).ok())
        .filter(|t| t.validate().is_ok())
        .unwrap_or_default()
}

/// 현재 토큰 위에 `patch`(부분 JSON)를 겹쳐 저장한다. 같은 값이면 아무것도 쓰지 않는다.
pub async fn update(ctx: &Ctx, actor: &Actor, patch: Value) -> Result<Tokens> {
    actor.require(Permission::DesignEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let (before, after) = merged(&mut tx, patch).await?;
    if after == before {
        return Ok(before);
    }
    let value = serde_json::to_value(&after).map_err(Error::internal)?;
    tx.put_setting(KEY, &value, &clock::now()).await?;
    tx.audit(
        actor,
        "design.tokens",
        Target::site(),
        json!({ "before": before, "after": after }),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 저장하지 않고 `patch` 를 겹친 토큰을 돌려준다. 화면 편집기의 디자인 판(ADR-0148)이 캔버스에 미리 입힐 때 쓴다.
/// 검사는 `update` 와 같다.
pub async fn preview(ctx: &Ctx, actor: &Actor, patch: Value) -> Result<Tokens> {
    actor.require(Permission::DesignEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let (_, after) = merged(&mut tx, patch).await?;
    Ok(after)
}

/// 저장된 토큰과, 그 위에 `patch` 를 겹쳐 검사한 토큰. 서체는 값이 바뀔 때만 DB 에서 확인한다.
async fn merged(tx: &mut Tx, patch: Value) -> Result<(Tokens, Tokens)> {
    if !patch.is_object() {
        return Err(Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", "객체가 아닙니다"));
    }
    let before = from_pairs(tx.settings().await?);
    let mut merged = serde_json::to_value(&before).map_err(Error::internal)?;
    merge(&mut merged, patch);
    let after: Tokens = serde_json::from_value(merged).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })?;
    after.validate()?;
    if after == before {
        return Ok((before, after));
    }
    // 내장 이름이 아닌 서체 값은 올린 서체여야 한다 (ADR-0048).
    for (field, value) in [
        ("font.body", &after.font.body),
        ("font.heading", &after.font.heading),
        ("font.mono", &after.font.mono),
    ] {
        if font_stack(value).is_none() && tx.font_by_slug(value).await?.is_none() {
            return Err(Error::new(Code::Validation)
                .with_message("error.font_not_found")
                .arg("slug", value.clone())
                .arg("field", field)
                .next("giwa font list"));
        }
    }
    Ok((before, after))
}

/// 기본값으로 되돌린다.
pub async fn reset(ctx: &Ctx, actor: &Actor) -> Result<Tokens> {
    let defaults = serde_json::to_value(Tokens::default()).map_err(Error::internal)?;
    update(ctx, actor, defaults).await
}

/// 테마로 입힐 패치. 테마가 정하는 칸(색·모서리·폭·제목 서체)은 기본값 위에 테마를 겹친 값으로 채워,
/// 앞서 입힌 테마의 색이 남지 않게 한다. 본문 서체·글자 크기·간격·배치는 건드리지 않는다.
pub fn theme_patch(theme: &Value) -> Value {
    let d = Tokens::default();
    let mut patch = json!({
        "color": d.color,
        "color_dark": d.color_dark,
        "radius": d.radius,
        "layout": { "max_width": d.layout.max_width },
        "font": { "heading": d.font.heading },
    });
    merge(&mut patch, theme.clone());
    patch
}

/// JSON 병합 (RFC 7396 과 같되 `null` 은 무시한다).
fn merge(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    continue;
                }
                match t.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        t.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_untouched_fields() {
        let mut base = serde_json::to_value(Tokens::default()).unwrap();
        merge(&mut base, json!({ "color": { "primary": "#123456" } }));
        let t: Tokens = serde_json::from_value(base).unwrap();
        assert_eq!(t.color.primary, "#123456");
        assert_eq!(t.color.bg, Tokens::default().color.bg);
    }

    #[test]
    fn theme_patch_clears_previous_theme() {
        // 따뜻한 테마를 입힌 뒤 색 하나만 정한 테마로 바꾸면 바탕은 기본값으로 돌아온다.
        let mut site = serde_json::to_value(Tokens::default()).unwrap();
        merge(
            &mut site,
            theme_patch(&json!({ "color": { "bg": "#fbf8f2" }, "font": { "heading": "serif" } })),
        );
        merge(
            &mut site,
            theme_patch(&json!({ "color": { "primary": "#2754c5" } })),
        );
        let t: Tokens = serde_json::from_value(site).unwrap();
        assert_eq!(t.color.primary, "#2754c5");
        assert_eq!(t.color.bg, Tokens::default().color.bg);
        assert_eq!(t.font.heading, Tokens::default().font.heading);
    }

    #[test]
    fn broken_stored_value_falls_back_to_default() {
        let t = from_pairs(vec![(KEY.into(), json!({ "color": 1 }))]);
        assert_eq!(t, Tokens::default());
    }
}
