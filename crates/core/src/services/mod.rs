//! 서비스 레이어. REST·CLI·웹·MCP 가 모두 여기만 부른다 (원칙 1).
//! 시그니처는 `(&Ctx, &Actor, input)`.

pub mod auth;
pub mod board_posts;
pub mod boards;
pub mod captcha;
pub mod categories;
pub mod comments;
pub mod design;
pub mod files;
pub mod fonts;
pub mod mail;
pub mod media;
pub mod members;
pub mod menus;
pub mod pages;
pub mod posts;
pub mod presets;
pub mod search;
pub mod settings;
pub mod tags;
pub mod templates;
pub mod tokens;
pub mod views;

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

/// 본문 Markdown 을 저장 형태로 맞춘다: 크기 검사, BOM 제거, 줄바꿈을 `\n` 하나로.
/// 브라우저 폼은 textarea 줄바꿈을 `\r\n` 으로 보낸다. 경로마다 다르게 저장되지 않게 여기서 통일한다.
pub(crate) fn body_text(body: String, max_bytes: usize) -> Result<String> {
    if body.len() > max_bytes {
        return Err(Error::new(Code::Validation)
            .with_message("error.body_too_large")
            .arg("max", max_bytes.to_string()));
    }
    let body = body.trim_start_matches('\u{feff}');
    Ok(if body.contains('\r') {
        body.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        body.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::body_text;

    #[test]
    fn body_line_endings_are_unified() {
        assert_eq!(
            body_text("\u{feff}가\r\n나\r다\n".into(), 100).unwrap(),
            "가\n나\n다\n"
        );
        assert!(body_text("가".repeat(40), 100).is_err());
    }
}
