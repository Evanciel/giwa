//! 금지어와 도배 방지 (GAP M9, ADR-0071). 쓰기를 막아야 하므로 모듈이 아니라 코어에 둔다.
//!
//! 공개 화면으로 쓰는 사람(손님·회원)에게만 건다. 관리자, 에이전트, 로컬 CLI 는 걸리지 않는다.
//! 손님은 시간 간격을 재지 않는다: 이어 볼 신원이 없고, IP 분당 한도가 이미 막는다.

use crate::actor::{Actor, ActorKind, Permission};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::SiteSettings;
use crate::store::Ctx;

pub const MAX_WORDS: usize = 500;
pub const MAX_WORD_CHARS: usize = 50;
pub const MAX_FLOOD_SECONDS: u32 = 3600;
/// 같은 댓글을 다시 쓸 수 없는 시간
const SAME_COMMENT_SECONDS: i64 = 3600;

fn exempt(actor: &Actor) -> bool {
    !matches!(actor.kind(), ActorKind::Anonymous | ActorKind::Human { .. })
        || actor.has(Permission::PostModerate)
}

/// 비교용 글자: 소문자로, 글자와 숫자만. 「바 보」「바.보」도 「바보」로 걸린다.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 설정 칸의 글을 금지어 목록으로. 줄바꿈이나 쉼표로 나눈다.
pub fn parse_words(raw: &str) -> Result<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    for word in raw.split(['\n', '\r', ',']).map(str::trim) {
        if normalize(word).is_empty() || words.iter().any(|w| w == word) {
            continue;
        }
        if word.chars().count() > MAX_WORD_CHARS {
            return Err(Error::new(Code::Validation)
                .with_message("error.banned_word_long")
                .arg("max", MAX_WORD_CHARS.to_string()));
        }
        words.push(word.to_owned());
    }
    if words.len() > MAX_WORDS {
        return Err(Error::new(Code::Validation)
            .with_message("error.banned_words_many")
            .arg("max", MAX_WORDS.to_string()));
    }
    Ok(words)
}

/// 금지어가 들어 있으면 거절한다. 무엇이 걸렸는지 말해 고칠 수 있게 한다.
pub fn check_text(site: &SiteSettings, actor: &Actor, texts: &[&str]) -> Result<()> {
    if site.banned_words.is_empty() || exempt(actor) {
        return Ok(());
    }
    let texts: Vec<String> = texts.iter().map(|t| normalize(t)).collect();
    for word in &site.banned_words {
        let key = normalize(word);
        if !key.is_empty() && texts.iter().any(|t| t.contains(&key)) {
            return Err(Error::new(Code::Validation)
                .with_message("error.banned_word")
                .arg("word", word.clone()));
        }
    }
    Ok(())
}

/// 회원이 방금 글이나 댓글을 썼으면 거절한다.
pub async fn check_flood(ctx: &Ctx, site: &SiteSettings, actor: &Actor) -> Result<()> {
    let Some(member) = actor.member_id().filter(|_| !exempt(actor)) else {
        return Ok(());
    };
    if site.flood_seconds == 0 {
        return Ok(());
    }
    let limit = i64::from(site.flood_seconds);
    let last = ctx.store.last_member_write(member).await?;
    match last.as_deref().and_then(clock::seconds_since) {
        Some(passed) if passed < limit => Err(Error::new(Code::RateLimited)
            .with_message("error.flood")
            .arg("seconds", (limit - passed).to_string())),
        _ => Ok(()),
    }
}

/// 바로 앞 댓글과 같은 글은 거절한다. 도배 방지가 켜져 있을 때만.
pub async fn check_same_comment(
    ctx: &Ctx,
    site: &SiteSettings,
    actor: &Actor,
    body: &str,
) -> Result<()> {
    let Some(member) = actor.member_id().filter(|_| !exempt(actor)) else {
        return Ok(());
    };
    if site.flood_seconds == 0 {
        return Ok(());
    }
    let Some((last, at)) = ctx.store.last_member_comment(member).await? else {
        return Ok(());
    };
    let recent = clock::seconds_since(&at).is_some_and(|s| s < SAME_COMMENT_SECONDS);
    if recent && last == body {
        return Err(Error::new(Code::Validation).with_message("error.same_comment"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn spacing_and_symbols_do_not_hide_a_word() {
        assert_eq!(normalize("바 보-AbC"), "바보abc");
        assert_eq!(
            parse_words("바보, 바보\n나쁜말\r\n ... ,").unwrap(),
            vec!["바보".to_owned(), "나쁜말".to_owned()]
        );
    }
}
