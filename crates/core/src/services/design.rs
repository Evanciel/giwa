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
    if !patch.is_object() {
        return Err(Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", "객체가 아닙니다"));
    }
    let mut tx = Tx::begin(ctx).await?;
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
        return Ok(before);
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
                .next("maru font list"));
        }
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

/// 기본값으로 되돌린다.
pub async fn reset(ctx: &Ctx, actor: &Actor) -> Result<Tokens> {
    let defaults = serde_json::to_value(Tokens::default()).map_err(Error::internal)?;
    update(ctx, actor, defaults).await
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
    fn broken_stored_value_falls_back_to_default() {
        let t = from_pairs(vec![(KEY.into(), json!({ "color": 1 }))]);
        assert_eq!(t, Tokens::default());
    }
}
