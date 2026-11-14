//! 태그 (GAP C2). 사이트 전체에 하나의 태그 목록이 있다.
//!
//! 태그는 글을 저장할 때 생긴다. 따로 만들거나 지우는 명령은 없고,
//! 글이 하나도 없는 태그는 목록에 나오지 않는다.

use crate::actor::{Actor, Permission};
use crate::error::{Code, Error, Result};
use crate::model::Tag;
use crate::slug;
use crate::store::{Ctx, WriteTx};

pub const MAX_PER_POST: usize = 20;
pub const MAX_CHARS: usize = 30;

/// 공개 글이 달린 태그 (이름순, 글 수 포함).
pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<Tag>> {
    if !actor.has(Permission::PostRead) {
        return Ok(vec![]);
    }
    ctx.store.list_tags(actor.rank()).await
}

/// 공개 태그 하나. 공개 글이 없는 태그도 찾는다(목록이 비어 보일 뿐이다).
pub async fn find(ctx: &Ctx, slug: &str) -> Result<Tag> {
    let key = slug::tag(slug);
    if key.is_empty() {
        return Err(not_found(slug));
    }
    ctx.store
        .tag_by_slug(&key)
        .await?
        .ok_or_else(|| not_found(slug))
}

/// 입력 태그를 정리한다: 앞 `#`·앞뒤 공백 제거, 안쪽 공백은 하나로, 빈 값과
/// 같은 주소의 중복은 빼고 순서는 지킨다. 이미 있는 태그면 그 이름으로 맞춘다.
pub(crate) async fn normalize(tx: &mut dyn WriteTx, input: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in input {
        let name = raw
            .trim()
            .trim_start_matches('#')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if name.is_empty() {
            continue;
        }
        if name.chars().count() > MAX_CHARS {
            return Err(Error::new(Code::Validation)
                .with_message("error.tag_length")
                .arg("value", name)
                .arg("max", MAX_CHARS.to_string()));
        }
        let key = slug::tag(&name);
        if key.is_empty() {
            return Err(Error::new(Code::Validation)
                .with_message("error.tag_invalid")
                .arg("value", name));
        }
        if out.iter().any(|(k, _)| *k == key) {
            continue;
        }
        out.push((key, name));
    }
    if out.len() > MAX_PER_POST {
        return Err(Error::new(Code::Validation)
            .with_message("error.tag_too_many")
            .arg("max", MAX_PER_POST.to_string()));
    }
    let mut names = Vec::with_capacity(out.len());
    for (key, name) in out {
        names.push(match tx.tag_by_slug(&key).await? {
            Some(t) => t.name,
            None => name,
        });
    }
    Ok(names)
}

fn not_found(slug: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.tag_not_found")
        .arg("slug", slug)
        .next("giwa tag list")
}
