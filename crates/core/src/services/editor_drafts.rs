//! 화면 편집기 초안 (ADR-0152). 사람마다 화면마다 한 벌을 서버에 둔다. 다른 기기·브라우저에서 이어 고친다.
//!
//! 초안은 그 사람의 작업 중 사본이라 사이트를 바꾸지 않는다. 그래서 감사 기록에 남기지 않고(세션의 마지막 사용
//! 시각과 같다), 발행이 변경 묶음과 감사 기록에 남는다. 사람은 자기 것만 읽고 고친다. 에이전트는 화면마다
//! 사람들의 초안을 읽기만 한다(거들 수 있게).

use serde_json::Value;

use crate::actor::{Actor, Permission};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::EditorDraft;
use crate::store::Ctx;

/// 초안 한 벌의 크기 한도(직렬화한 본문). 블록 300개(편집기 한도)에 넉넉하다.
pub const MAX_BYTES: usize = 512 * 1024;

/// 화면 키(`home`·`page:<slug>`·`board:<slug>`)의 모양만 본다. 어떤 화면인지는 편집기가 안다.
/// 주소에는 한글이 올 수 있다(`page:메뉴`). 편집기의 화면 주소 규칙(글자·숫자·`-`·`_`, 120바이트)과 맞춘다.
fn check_screen(screen: &str) -> Result<()> {
    let ok = !screen.is_empty()
        && screen.len() <= 128
        && screen
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ':' | '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.draft_screen")
            .arg("value", screen.chars().take(64).collect::<String>()))
    }
}

/// 초안의 주인. 화면 편집 권한이 있는 사람만.
fn owner(actor: &Actor) -> Result<&str> {
    actor.require(Permission::DesignEdit)?;
    actor
        .member_id()
        .map(String::as_str)
        .ok_or_else(|| Error::new(Code::Forbidden).with_message("error.draft_human_only"))
}

/// 내 초안. 없으면 `None`.
pub async fn get(ctx: &Ctx, actor: &Actor, screen: &str) -> Result<Option<EditorDraft>> {
    let me = owner(actor)?;
    check_screen(screen)?;
    ctx.store.editor_draft(me, screen).await
}

/// 내 초안을 덮어쓴다. `base` 는 편집기가 연 때의 사이트 값 지문이다(그 사이 발행이 있었는지 견준다).
pub async fn save(
    ctx: &Ctx,
    actor: &Actor,
    screen: &str,
    body: Value,
    base: &str,
) -> Result<EditorDraft> {
    let me = owner(actor)?.to_owned();
    check_screen(screen)?;
    if !body.is_object() {
        return Err(Error::new(Code::Validation).with_message("error.draft_body"));
    }
    let size = serde_json::to_string(&body).map_err(Error::internal)?.len();
    if size > MAX_BYTES || base.len() > 64 {
        return Err(Error::new(Code::Validation)
            .with_message("error.draft_too_large")
            .arg("max", (MAX_BYTES / 1024).to_string()));
    }
    let draft = EditorDraft {
        member_id: me,
        screen: screen.to_owned(),
        body,
        base: base.to_owned(),
        updated_at: clock::now(),
    };
    ctx.store.put_editor_draft(&draft).await?;
    Ok(draft)
}

/// 내 초안을 버린다. 없었으면 `false`(여러 번 불러도 같다).
pub async fn discard(ctx: &Ctx, actor: &Actor, screen: &str) -> Result<bool> {
    let me = owner(actor)?;
    check_screen(screen)?;
    ctx.store.delete_editor_draft(me, screen).await
}

/// 이 화면의 모든 사람 초안, 최근 것부터. 화면 편집 권한이 있으면 사람·에이전트 모두 읽는다.
pub async fn for_screen(ctx: &Ctx, actor: &Actor, screen: &str) -> Result<Vec<EditorDraft>> {
    actor.require(Permission::DesignEdit)?;
    check_screen(screen)?;
    ctx.store.editor_drafts(screen).await
}
