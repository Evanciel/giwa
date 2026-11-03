//! 자체 사람 확인 질문 (GAP M10, ADR-0052). 외부 서비스·그림 없이 한글 셈 문제를 낸다.
//!
//! 화면 낭독기로도 읽을 수 있고, 답은 숫자다. 서버가 만든 값의 해시만 두고 한 번만 쓴다.
//! 셈을 풀 줄 아는 자동 프로그램은 통과한다 — 막는 것은 아무 폼에나 글을 붓는 단순한 봇이다.
//! 받은 지 [`MIN_SECONDS`] 안에 낸 답은 사람이 아닌 것으로 본다.

use serde::{Deserialize, Serialize};
use time::Duration;

use crate::clock;
use crate::error::{Code, Error, Result};
use crate::services::tokens;
use crate::store::Ctx;

pub const TTL_MINUTES: i64 = 10;
pub const MIN_SECONDS: i64 = 2;

const WORDS: [&str; 10] = [
    "하나", "둘", "셋", "넷", "다섯", "여섯", "일곱", "여덟", "아홉", "열",
];

/// 화면에 낼 질문. `token` 은 폼의 숨은 칸에 넣는다.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Challenge {
    pub token: String,
    pub question: String,
}

/// 사람이 낸 답.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct Answer {
    pub token: String,
    pub answer: String,
}

pub async fn issue(ctx: &Ctx) -> Result<Challenge> {
    issue_for(ctx, TTL_MINUTES).await
}

/// 오래 걸리는 폼(글쓰기)은 유효 시간을 길게.
pub async fn issue_for(ctx: &Ctx, minutes: i64) -> Result<Challenge> {
    let raw = crate::services::auth::new_secret()?;
    let bytes = hex::decode(&raw[..4]).map_err(|e| Error::internal(e.to_string()))?;
    let a = usize::from(bytes[0] % 10);
    let b = usize::from(bytes[1] % 10);
    // 빼기는 큰 수에서 작은 수를 뺀다 (답이 음수가 되지 않게).
    let (question, answer) = if bytes[0] & 0x80 == 0 {
        (
            format!("{} 더하기 {}{} 얼마일까요?", WORDS[a], WORDS[b], topic(b)),
            a + b + 2,
        )
    } else {
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        (
            format!("{} 빼기 {}{} 얼마일까요?", WORDS[hi], WORDS[lo], topic(lo)),
            hi - lo,
        )
    };
    let now = time::OffsetDateTime::now_utc();
    ctx.store
        .insert_challenge(
            &tokens::hash(&raw),
            &answer.to_string(),
            &clock::format(now),
            &clock::format(now + Duration::minutes(minutes)),
        )
        .await?;
    Ok(Challenge {
        token: raw,
        question,
    })
}

/// 맞으면 `Ok`. 틀려도 도전은 쓰인다 — 화면은 새 질문을 다시 낸다.
pub async fn check(ctx: &Ctx, answer: &Answer) -> Result<()> {
    let wrong = || {
        Error::new(Code::Validation)
            .with_message("error.captcha_wrong")
            .next("질문을 새로 받아 숫자로 답하세요")
    };
    let token = answer.token.trim();
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(wrong());
    }
    let now = time::OffsetDateTime::now_utc();
    let Some((expected, issued_at)) = ctx
        .store
        .take_challenge(&tokens::hash(token), &clock::format(now))
        .await?
    else {
        return Err(wrong());
    };
    let too_fast =
        clock::parse(&issued_at).is_none_or(|t| now - t < Duration::seconds(MIN_SECONDS));
    if too_fast || normalize(&answer.answer) != expected {
        return Err(wrong());
    }
    Ok(())
}

/// 받침이 없는 「하나」 만 「는」, 나머지는 「은」.
fn topic(i: usize) -> &'static str {
    if i == 0 { "는" } else { "은" }
}

/// 전각 숫자·앞뒤 공백을 받아 준다.
fn normalize(s: &str) -> String {
    s.trim()
        .chars()
        .filter_map(|c| match c {
            '0'..='9' => Some(c),
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_normalized() {
        assert_eq!(normalize(" 12 "), "12");
        assert_eq!(normalize("１２"), "12");
        assert_eq!(normalize("열둘"), "");
    }
}
