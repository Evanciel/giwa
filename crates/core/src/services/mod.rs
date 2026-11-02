//! 서비스 레이어. REST·CLI·웹·MCP 가 모두 여기만 부른다 (원칙 1).
//! 시그니처는 `(&Ctx, &Actor, input)`.

pub mod boards;
pub mod design;
pub mod menus;
pub mod pages;
pub mod posts;
pub mod presets;
pub mod settings;
pub mod tokens;

use crate::actor::Actor;
use crate::error::{Code, Error, Result};
use crate::store::Ctx;

/// 비로그인 방문자. 권한은 DB 의 `guest` 역할을 따른다.
pub async fn anonymous_actor(ctx: &Ctx) -> Result<Actor> {
    let role = ctx.store.role_by_name("guest").await?;
    Ok(Actor::anonymous(
        role.map(|r| r.permissions).unwrap_or_default(),
    ))
}

/// 앞뒤 공백을 지우고 길이를 검사한다.
pub(crate) fn required_text(field: &'static str, value: &str, max_chars: usize) -> Result<String> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > max_chars {
        return Err(Error::new(Code::Validation)
            .with_message("error.field_length")
            .arg("field", field)
            .arg("max", max_chars.to_string()));
    }
    Ok(v.to_owned())
}
