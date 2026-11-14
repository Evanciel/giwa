//! 글 틀. 새 글을 쓸 때 불러오는 본문 뼈대.
//!
//! 기본 틀(여행기·리뷰·공지·레시피)은 바이너리에 있고, 사용자가 저장한 틀은 DB 에 있다.
//! 저장은 이름 기준으로 덮어쓴다(같은 명령을 여러 번 실행해도 틀은 하나).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::PostTemplate;
use crate::services::{body_text, required_text};
use crate::store::{Ctx, Tx};

const MAX_NAME: usize = 60;
const MAX_BODY_BYTES: usize = 64 * 1024;
const BUILTIN_PREFIX: &str = "builtin-";

/// (id 꼬리, 이름, 본문). 문법은 에디터 서식 모드로 열리는 것만 쓴다.
const BUILTIN: [(&str, &str, &str); 4] = [
    (
        "travel",
        "여행기",
        "## 한 줄 요약\n\n여기에 한 줄로 적어 보세요.\n\n## 일정\n\n| 날짜 | 장소 | 메모 |\n| --- | --- | --- |\n| 1일차 |  |  |\n| 2일차 |  |  |\n\n## 좋았던 점\n\n- \n\n## 아쉬웠던 점\n\n- \n\n## 쓴 돈\n\n| 항목 | 금액 |\n| --- | ---: |\n| 숙소 |  |\n| 교통 |  |\n| 식비 |  |\n",
    ),
    (
        "review",
        "리뷰",
        "## 한 줄 평\n\n여기에 한 줄로 적어 보세요.\n\n:::tip\n이런 분께 추천해요: \n:::\n\n## 좋은 점\n\n- \n\n## 아쉬운 점\n\n- \n\n## 총평\n\n별점: ★★★★☆\n",
    ),
    (
        "notice",
        "공지",
        ":::info\n**알려 드립니다.** 여기에 핵심을 한 줄로 적어 주세요.\n:::\n\n## 내용\n\n\n\n## 일정·문의\n\n- 일시: \n- 장소: \n- 문의: \n",
    ),
    (
        "recipe",
        "레시피",
        "## 재료 (2인분)\n\n- [ ] \n- [ ] \n\n## 만드는 법\n\n1. \n2. \n\n:::tip\n맛있게 먹는 팁: \n:::\n",
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveTemplate {
    /// 틀 이름. 같은 이름이 있으면 본문을 덮어쓴다
    pub name: String,
    pub body_md: String,
}

fn builtins() -> impl Iterator<Item = PostTemplate> {
    BUILTIN.iter().map(|(id, name, body)| PostTemplate {
        id: format!("{BUILTIN_PREFIX}{id}"),
        name: (*name).to_owned(),
        body_md: (*body).to_owned(),
        builtin: true,
        created_at: String::new(),
        updated_at: String::new(),
    })
}

/// 기본 틀 다음에 저장한 틀 (이름순).
pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<PostTemplate>> {
    actor.require(Permission::PostWrite)?;
    let mut out: Vec<PostTemplate> = builtins().collect();
    out.extend(ctx.store.list_templates().await?);
    Ok(out)
}

/// 이름 기준으로 만들거나 덮어쓴다. 기본 틀과 같은 이름은 쓸 수 없다.
pub async fn save(ctx: &Ctx, actor: &Actor, input: SaveTemplate) -> Result<PostTemplate> {
    actor.require(Permission::PostPublish)?;
    let name = required_text("name", &input.name, MAX_NAME)?;
    if BUILTIN.iter().any(|b| b.1 == name) {
        return Err(Error::new(Code::Conflict)
            .with_message("error.post_template_builtin")
            .arg("name", name));
    }
    let body_md = body_text(input.body_md, MAX_BODY_BYTES)?;
    let now = clock::now();
    let mut tx = Tx::begin(ctx).await?;
    let before = tx.template_by_name(&name).await?;
    let after = match &before {
        Some(b) if b.body_md == body_md => return Ok(b.clone()),
        Some(b) => PostTemplate {
            body_md,
            updated_at: now,
            ..b.clone()
        },
        None => PostTemplate {
            id: clock::new_id(),
            name,
            body_md,
            builtin: false,
            created_at: now.clone(),
            updated_at: now,
        },
    };
    if before.is_some() {
        tx.update_template(&after).await?;
    } else {
        tx.insert_template(&after).await?;
    }
    tx.audit(
        actor,
        if before.is_some() {
            "template.update"
        } else {
            "template.create"
        },
        Target::new("template", &after.id),
        audit::diff(before.as_ref(), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// ID 나 이름으로 지운다. 기본 틀은 지울 수 없다.
pub async fn delete(ctx: &Ctx, actor: &Actor, id_or_name: &str) -> Result<PostTemplate> {
    actor.require(Permission::PostPublish)?;
    if id_or_name.starts_with(BUILTIN_PREFIX) || BUILTIN.iter().any(|b| b.1 == id_or_name) {
        return Err(Error::new(Code::Conflict)
            .with_message("error.post_template_builtin")
            .arg("name", id_or_name));
    }
    let mut tx = Tx::begin(ctx).await?;
    let found = match tx.template_by_id(id_or_name).await? {
        Some(t) => Some(t),
        None => tx.template_by_name(id_or_name.trim()).await?,
    };
    let before = found.ok_or_else(|| {
        Error::new(Code::NotFound)
            .with_message("error.post_template_not_found")
            .arg("name", id_or_name)
            .next("giwa template list")
    })?;
    tx.delete_template(&before.id).await?;
    tx.audit(
        actor,
        "template.delete",
        Target::new("template", &before.id),
        audit::diff(Some(&before), None),
    )
    .await?;
    tx.commit().await?;
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_ids_are_stable_and_unique() {
        let all: Vec<_> = builtins().collect();
        assert_eq!(all.len(), BUILTIN.len());
        assert!(
            all.iter()
                .all(|t| t.id.starts_with(BUILTIN_PREFIX) && t.builtin)
        );
        let mut names: Vec<_> = all.iter().map(|t| t.name.as_str()).collect();
        names.dedup();
        assert_eq!(names.len(), BUILTIN.len());
    }
}
