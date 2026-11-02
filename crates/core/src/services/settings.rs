//! 사이트 설정. `site_settings` 에 `site.<필드>` 키로 하나씩 저장한다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::SiteSettings;
use crate::services::required_text;
use crate::store::{Ctx, Tx};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub tz_offset_min: Option<i32>,
    #[serde(default)]
    pub posts_per_page: Option<u32>,
    #[serde(default)]
    pub naver_verification: Option<String>,
    #[serde(default)]
    pub google_verification: Option<String>,
}

/// 저장된 값 위에 기본값을 채운다. 모르는 키나 잘못된 값은 기본값으로 둔다.
pub async fn site(ctx: &Ctx) -> Result<SiteSettings> {
    Ok(from_pairs(ctx.store.settings().await?))
}

fn from_pairs(pairs: Vec<(String, Value)>) -> SiteSettings {
    let mut map = serde_json::to_value(SiteSettings::default())
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (key, value) in pairs {
        if let Some(field) = key.strip_prefix("site.")
            && map.contains_key(field)
        {
            map.insert(field.to_owned(), value);
        }
    }
    serde_json::from_value(Value::Object(map)).unwrap_or_default()
}

pub async fn update(ctx: &Ctx, actor: &Actor, patch: SettingsPatch) -> Result<SiteSettings> {
    actor.require(Permission::StructureEdit)?;
    let mut changes: Vec<(&'static str, Value)> = Vec::new();
    if let Some(n) = &patch.name {
        changes.push(("name", json!(required_text("name", n, 80)?)));
    }
    if let Some(d) = &patch.description {
        let d = d.trim();
        if d.chars().count() > 300 {
            return Err(Error::new(Code::Validation)
                .with_message("error.field_length")
                .arg("field", "description")
                .arg("max", "300"));
        }
        changes.push(("description", json!(d)));
    }
    if let Some(l) = &patch.lang {
        let ok = !l.is_empty()
            && l.len() <= 20
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok {
            return Err(Error::new(Code::Validation)
                .with_message("error.setting_lang")
                .arg("value", l.clone()));
        }
        changes.push(("lang", json!(l)));
    }
    if let Some(tz) = patch.tz_offset_min {
        if !(-720..=840).contains(&tz) {
            return Err(Error::new(Code::Validation)
                .with_message("error.setting_tz")
                .arg("value", tz.to_string()));
        }
        changes.push(("tz_offset_min", json!(tz)));
    }
    if let Some(n) = patch.posts_per_page {
        changes.push(("posts_per_page", json!(n.clamp(1, 100))));
    }
    for (key, v) in [
        ("naver_verification", &patch.naver_verification),
        ("google_verification", &patch.google_verification),
    ] {
        if let Some(v) = v {
            let v = v.trim();
            let ok = v.len() <= 100
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !ok {
                return Err(Error::new(Code::Validation)
                    .with_message("error.setting_verification")
                    .arg("field", key)
                    .arg("value", v.chars().take(40).collect::<String>()));
            }
            changes.push((key, json!(v)));
        }
    }

    let mut tx = Tx::begin(ctx).await?;
    let before = from_pairs(tx.settings().await?);
    let mut after_value = serde_json::to_value(&before).map_err(Error::internal)?;
    for (k, v) in &changes {
        after_value[*k] = v.clone();
    }
    let after: SiteSettings = serde_json::from_value(after_value).map_err(Error::internal)?;
    if after == before {
        return Ok(before);
    }
    let now = clock::now();
    for (k, v) in &changes {
        tx.put_setting(&format!("site.{k}"), v, &now).await?;
    }
    tx.audit(
        actor,
        "settings.update",
        Target::site(),
        json!({ "before": before, "after": after }),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_missing_and_bad_values() {
        let s = from_pairs(vec![
            ("site.name".into(), json!("내 블로그")),
            ("site.unknown".into(), json!(1)),
        ]);
        assert_eq!(s.name, "내 블로그");
        assert_eq!(s.lang, "ko");
        // 타입이 틀린 값이 있으면 전체 기본값으로 (화면이 깨지지 않게)
        let s = from_pairs(vec![("site.tz_offset_min".into(), json!("x"))]);
        assert_eq!(s, SiteSettings::default());
    }
}
