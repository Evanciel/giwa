//! 페이지 서비스. 페이지는 사이트 구조라 `structure.edit` 권한으로 쓴다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{PAGE_LAYOUTS, Page, PageStatus, Revision};
use crate::services::boards::Created;
use crate::services::required_text;
use crate::slug;
use crate::store::{Ctx, Tx};

const MAX_TITLE: usize = 200;
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPage {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub body_md: String,
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default)]
    pub sort_order: i64,
}

fn default_layout() -> String {
    "default".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PagePatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub body_md: Option<String>,
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

pub async fn create(ctx: &Ctx, actor: &Actor, input: NewPage) -> Result<Page> {
    Ok(create_inner(ctx, actor, input, false).await?.item)
}

/// 같은 슬러그가 있으면 그대로 돌려준다.
pub async fn ensure(ctx: &Ctx, actor: &Actor, input: NewPage) -> Result<Created<Page>> {
    create_inner(ctx, actor, input, true).await
}

async fn create_inner(
    ctx: &Ctx,
    actor: &Actor,
    input: NewPage,
    if_not_exists: bool,
) -> Result<Created<Page>> {
    actor.require(Permission::StructureEdit)?;
    let author_type = actor
        .author_type()
        .ok_or_else(|| actor.denied(Permission::StructureEdit))?;
    let slug = slug::normalize(&input.slug)?;
    let title = required_text("title", &input.title, MAX_TITLE)?;
    let body_md = check_body(input.body_md)?;
    check_layout(&input.layout)?;

    let mut tx = Tx::begin(ctx).await?;
    if let Some(existing) = tx.page_by_slug(&slug).await? {
        if if_not_exists {
            return Ok(Created {
                created: false,
                item: existing,
            });
        }
        return Err(Error::new(Code::Conflict)
            .with_message("error.page_exists")
            .arg("slug", slug.clone())
            .next(format!("maru page edit {slug}")));
    }
    let now = clock::now();
    let page = Page {
        id: clock::new_id(),
        slug,
        title,
        body_md,
        status: PageStatus::Draft,
        layout: input.layout,
        author_id: actor.member_id().cloned(),
        actor_type: author_type,
        sort_order: input.sort_order,
        published_at: None,
        created_at: now.clone(),
        updated_at: now,
    };
    tx.insert_page(&page).await?;
    tx.insert_revision(&revision_of(&page, actor)).await?;
    tx.audit(
        actor,
        "page.create",
        Target::new("page", &page.id),
        audit::diff(None, Some(&page)),
    )
    .await?;
    tx.commit().await?;
    Ok(Created {
        created: true,
        item: page,
    })
}

pub async fn update(ctx: &Ctx, actor: &Actor, slug_: &str, patch: PagePatch) -> Result<Page> {
    actor.require(Permission::StructureEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx
        .page_by_slug(slug_)
        .await?
        .ok_or_else(|| not_found(slug_))?;
    if before.status == PageStatus::Deleted {
        return Err(bad_transition(&before, PageStatus::Deleted));
    }
    let mut after = before.clone();
    if let Some(t) = &patch.title {
        after.title = required_text("title", t, MAX_TITLE)?;
    }
    if let Some(b) = patch.body_md {
        after.body_md = check_body(b)?;
    }
    if let Some(l) = patch.layout {
        check_layout(&l)?;
        after.layout = l;
    }
    if let Some(o) = patch.sort_order {
        after.sort_order = o;
    }
    if let Some(s) = &patch.slug {
        let s = slug::normalize(s)?;
        if s != before.slug && tx.page_by_slug(&s).await?.is_some() {
            return Err(Error::new(Code::Conflict)
                .with_message("error.page_exists")
                .arg("slug", s));
        }
        after.slug = s;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_page(&after).await?;
    if after.title != before.title || after.body_md != before.body_md {
        tx.insert_revision(&revision_of(&after, actor)).await?;
    }
    tx.audit(
        actor,
        "page.update",
        Target::new("page", &after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn publish(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    transition(
        ctx,
        actor,
        slug,
        &[PageStatus::Draft, PageStatus::Hidden],
        PageStatus::Published,
        "page.publish",
    )
    .await
}

pub async fn hide(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    transition(
        ctx,
        actor,
        slug,
        &[PageStatus::Published],
        PageStatus::Hidden,
        "page.hide",
    )
    .await
}

pub async fn delete(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    transition(
        ctx,
        actor,
        slug,
        &[PageStatus::Draft, PageStatus::Published, PageStatus::Hidden],
        PageStatus::Deleted,
        "page.delete",
    )
    .await
}

/// 삭제를 되돌린다. 발행된 적이 있으면 공개로, 아니면 초안으로.
pub async fn restore(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    actor.require(Permission::StructureEdit)?;
    let page = ctx
        .store
        .page_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    let to = if page.published_at.is_some() {
        PageStatus::Published
    } else {
        PageStatus::Draft
    };
    transition(ctx, actor, slug, &[PageStatus::Deleted], to, "page.restore").await
}

async fn transition(
    ctx: &Ctx,
    actor: &Actor,
    slug: &str,
    from: &[PageStatus],
    to: PageStatus,
    action: &'static str,
) -> Result<Page> {
    actor.require(Permission::StructureEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx
        .page_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    if before.status == to {
        return Ok(before);
    }
    if !from.contains(&before.status) {
        return Err(bad_transition(&before, to));
    }
    let now = clock::now();
    let published_at = if to == PageStatus::Published {
        before.published_at.clone().or_else(|| Some(now.clone()))
    } else {
        before.published_at.clone()
    };
    let after = Page {
        status: to,
        published_at,
        updated_at: now,
        ..before.clone()
    };
    tx.update_page(&after).await?;
    tx.audit(
        actor,
        action,
        Target::new("page", &after.id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 관리용 단건. 공개 페이지가 아니면 구조 편집 권한이 필요하다.
pub async fn get(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    let page = ctx
        .store
        .page_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    if page.status == PageStatus::Published && actor.has(Permission::SiteView) {
        return Ok(page);
    }
    actor
        .require(Permission::StructureEdit)
        .map_err(|_| not_found(slug))?;
    Ok(page)
}

pub async fn list(ctx: &Ctx, actor: &Actor, status: Option<PageStatus>) -> Result<Vec<Page>> {
    if !actor.has(Permission::StructureEdit) {
        return public_pages(ctx, actor).await;
    }
    ctx.store.list_pages(status).await
}

/// 공개 페이지 하나. 사이트맵·메뉴와 같은 공개 기준(`published`)을 쓴다.
pub async fn public_page(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    if !actor.has(Permission::SiteView) {
        return Err(not_found(slug));
    }
    let s = slug::normalize(slug).map_err(|_| not_found(slug))?;
    ctx.store
        .page_by_slug(&s)
        .await?
        .filter(|p| p.status == PageStatus::Published)
        .ok_or_else(|| not_found(slug))
}

pub async fn public_pages(ctx: &Ctx, actor: &Actor) -> Result<Vec<Page>> {
    if !actor.has(Permission::SiteView) {
        return Ok(vec![]);
    }
    ctx.store.list_pages(Some(PageStatus::Published)).await
}

fn revision_of(page: &Page, actor: &Actor) -> Revision {
    let (_, actor_id) = actor.audit_identity();
    Revision {
        id: clock::new_id(),
        post_id: None,
        page_id: Some(page.id.clone()),
        title: page.title.clone(),
        body_md: page.body_md.clone(),
        actor_type: page.actor_type,
        actor_id,
        created_at: page.updated_at.clone(),
    }
}

fn check_layout(layout: &str) -> Result<()> {
    if PAGE_LAYOUTS.contains(&layout) {
        return Ok(());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.page_layout")
        .arg("value", layout)
        .arg("allowed", PAGE_LAYOUTS.join(", ")))
}

fn check_body(body: String) -> Result<String> {
    if body.len() > MAX_BODY_BYTES {
        return Err(Error::new(Code::Validation)
            .with_message("error.body_too_large")
            .arg("max", MAX_BODY_BYTES.to_string()));
    }
    Ok(body.trim_start_matches('\u{feff}').to_owned())
}

pub(crate) fn not_found(slug: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.page_not_found")
        .arg("slug", slug)
        .next("maru page list")
}

fn bad_transition(page: &Page, to: PageStatus) -> Error {
    Error::new(Code::Conflict)
        .with_message("error.post_transition")
        .arg("from", page.status.as_str())
        .arg("to", to.as_str())
}
