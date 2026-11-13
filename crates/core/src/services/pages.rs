//! 페이지 서비스. 페이지는 사이트 구조라 `structure.edit` 권한으로 쓴다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{PAGE_LAYOUTS, Page, PageStatus, Revision};
use crate::services::boards::Created;
use crate::services::widgets::NewWidget;
use crate::services::{redirects, required_text, settings};
use crate::slug;
use crate::store::{Ctx, Tx};
use crate::urls::{self, Target as UrlTarget};

const MAX_TITLE: usize = 200;

/// 페이지 제목 검사. 화면 편집기가 발행 전에 같은 규칙으로 본다 (ADR-0160).
pub fn check_title(title: &str) -> Result<String> {
    required_text("title", title, MAX_TITLE)
}
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// 페이지 계층은 네 단계까지다. 그보다 깊으면 읽는 사람이 길을 잃는다.
const MAX_DEPTH: usize = 4;

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
    /// 위 페이지의 주소. 없으면 맨 위 (ADR-0128)
    #[serde(default)]
    pub parent: Option<String>,
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
    /// 위 페이지의 주소. 빈 값이면 맨 위로 (ADR-0128)
    #[serde(default)]
    pub parent: Option<String>,
    /// 검색 결과·공유 미리보기 설명. 빈 값이면 본문 앞부분 (ADR-0160)
    #[serde(default)]
    pub description: Option<String>,
    /// 공유 미리보기 그림 (`/media/…` 또는 https). 빈 값이면 사이트 공유 그림
    #[serde(default)]
    pub share_image: Option<String>,
    /// 검색 엔진에 내보내지 않는다
    #[serde(default)]
    pub noindex: Option<bool>,
}

/// 위 페이지를 찾고 고리·깊이를 본다. `this` 는 옮기는 페이지 (새로 만들면 없다).
async fn parent_id(tx: &mut Tx, this: Option<&str>, parent: &str) -> Result<Option<String>> {
    if parent.trim().is_empty() {
        return Ok(None);
    }
    let found = tx
        .page_by_slug(parent.trim())
        .await?
        .ok_or_else(|| not_found(parent.trim()))?;
    let mut depth = 1;
    let mut at = Some(found.clone());
    while let Some(p) = at {
        if this == Some(p.id.as_str()) {
            return Err(Error::new(Code::Validation)
                .with_message("error.page_parent_cycle")
                .arg("slug", parent.trim())
                .next("maru page list"));
        }
        depth += 1;
        if depth > MAX_DEPTH {
            return Err(Error::new(Code::Validation)
                .with_message("error.page_parent_deep")
                .arg("max", MAX_DEPTH.to_string())
                .next("maru page list"));
        }
        at = match &p.parent_id {
            Some(id) => tx.page_by_id(id).await?,
            None => None,
        };
    }
    Ok(Some(found.id))
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
    let parent = match &input.parent {
        Some(p) => parent_id(&mut tx, None, p).await?,
        None => None,
    };
    let now = clock::now();
    let page = Page {
        id: clock::new_id(),
        slug,
        title,
        body_md,
        status: PageStatus::Draft,
        layout: input.layout,
        parent_id: parent,
        author_id: actor.member_id().cloned(),
        actor_type: author_type,
        sort_order: input.sort_order,
        published_at: None,
        created_at: now.clone(),
        updated_at: now,
        description: String::new(),
        share_image: String::new(),
        noindex: false,
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
    if let Some(p) = &patch.parent {
        after.parent_id = parent_id(&mut tx, Some(&before.id), p).await?;
    }
    if let Some(d) = &patch.description {
        after.description = settings::check_description(d)?;
    }
    if let Some(u) = &patch.share_image {
        let u = u.trim();
        if !u.is_empty() && !settings::share_image_ok(u) {
            return Err(Error::new(Code::Validation)
                .with_message("error.share_image")
                .arg("value", u.chars().take(80).collect::<String>()));
        }
        after.share_image = u.to_owned();
    }
    if let Some(n) = patch.noindex {
        after.noindex = n;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_page(&after).await?;
    if after.slug != before.slug {
        let from = urls::path(&UrlTarget::Page { page: &before.slug });
        let to = urls::path(&UrlTarget::Page { page: &after.slug });
        redirects::moved(&mut tx, &from, &to).await?;
    }
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

/// 위 페이지의 주소. changeset 역연산이 되돌릴 값이다.
pub async fn parent_slug(ctx: &Ctx, page: &Page) -> Result<String> {
    Ok(match &page.parent_id {
        Some(id) => ctx
            .store
            .page_by_id(id)
            .await?
            .map(|p| p.slug)
            .unwrap_or_default(),
        None => String::new(),
    })
}

/// 페이지 하나로 가는 길.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NavLink {
    pub slug: String,
    pub title: String,
}

/// 문서 목록의 한 칸.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TreeNode {
    pub slug: String,
    pub title: String,
    pub children: Vec<TreeNode>,
}

/// 페이지 주변의 길 (ADR-0128): 위쪽 경로, 바로 아래 페이지, 문서 목록과 앞뒤 문서.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PageNav {
    pub trail: Vec<NavLink>,
    pub children: Vec<NavLink>,
    pub tree: Vec<TreeNode>,
    pub prev: Option<NavLink>,
    pub next: Option<NavLink>,
}

fn link(p: &Page) -> NavLink {
    NavLink {
        slug: p.slug.clone(),
        title: p.title.clone(),
    }
}

/// `all` 은 공개 페이지 전부(순서대로). 숨긴 위 페이지는 없는 셈 친다.
pub fn navigation(all: &[Page], current: &Page) -> PageNav {
    let by_id = |id: &str| all.iter().find(|p| p.id == id);
    let mut trail = Vec::new();
    let mut at = current.parent_id.as_deref().and_then(by_id);
    while let Some(p) = at {
        if trail.len() >= MAX_DEPTH {
            break;
        }
        trail.insert(0, link(p));
        at = p.parent_id.as_deref().and_then(by_id);
    }
    let children = all
        .iter()
        .filter(|p| p.parent_id.as_deref() == Some(current.id.as_str()))
        .map(link)
        .collect();
    let mut nav = PageNav {
        trail,
        children,
        ..PageNav::default()
    };
    if current.layout != "docs" {
        return nav;
    }
    let docs: Vec<&Page> = all.iter().filter(|p| p.layout == "docs").collect();
    let is_doc = |id: &str| docs.iter().any(|p| p.id == id);
    let roots: Vec<&Page> = docs
        .iter()
        .copied()
        .filter(|p| !p.parent_id.as_deref().is_some_and(is_doc))
        .collect();
    nav.tree = roots.iter().map(|p| tree_node(&docs, p, 1)).collect();
    let mut flat = Vec::new();
    flatten(&nav.tree, &mut flat);
    if let Some(i) = flat.iter().position(|n| n.slug == current.slug) {
        nav.prev = i.checked_sub(1).and_then(|j| flat.get(j).cloned());
        nav.next = flat.get(i + 1).cloned();
    }
    nav
}

fn tree_node(docs: &[&Page], page: &Page, depth: usize) -> TreeNode {
    let children = if depth < MAX_DEPTH {
        docs.iter()
            .filter(|p| p.parent_id.as_deref() == Some(page.id.as_str()))
            .map(|p| tree_node(docs, p, depth + 1))
            .collect()
    } else {
        Vec::new()
    };
    TreeNode {
        slug: page.slug.clone(),
        title: page.title.clone(),
        children,
    }
}

fn flatten(nodes: &[TreeNode], out: &mut Vec<NavLink>) {
    for n in nodes {
        out.push(NavLink {
            slug: n.slug.clone(),
            title: n.title.clone(),
        });
        flatten(&n.children, out);
    }
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
    super::body_text(body, MAX_BODY_BYTES)
}

// ---- 새 페이지 틀 (ADR-0149) ----

/// 새 페이지 틀. 목록은 테마가 준다(`maru_themes::starters`). 화면 편집기·CLI·MCP 가 같은 틀을 쓴다.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Starter {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub blocks: Vec<StarterBlock>,
    /// 글로 된 틀(이용약관·환불 규정, ADR-0167). 있으면 블록 대신 이 본문으로 보통 페이지를 만든다.
    /// `{title}` 은 페이지 이름이 된다
    #[serde(default)]
    pub body_md: String,
}

/// 틀의 블록 하나. 놓일 곳(`scope`)은 만들 페이지로 정해진다.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StarterBlock {
    pub kind: String,
    pub region: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub config: Value,
}

/// 틀의 설정에서 페이지 이름으로 바뀌는 자리.
const TITLE_SLOT: &str = "{title}";

impl Starter {
    /// 이 틀의 블록을 `slug` 페이지에 놓을 위젯 입력으로 바꾼다. 설정의 `"{title}"` 은 페이지 이름이 된다.
    pub fn widgets(&self, slug: &str, title: &str) -> Vec<NewWidget> {
        self.blocks
            .iter()
            .map(|b| {
                let mut config = b.config.clone();
                if let Some(obj) = config.as_object_mut() {
                    for v in obj.values_mut() {
                        if v.as_str() == Some(TITLE_SLOT) {
                            *v = Value::String(title.to_owned());
                        }
                    }
                }
                NewWidget {
                    kind: b.kind.clone(),
                    region: b.region.clone(),
                    scope: format!("page:{slug}"),
                    title: b.title.clone(),
                    config,
                    position: None,
                    enabled: true,
                    hide_on: Vec::new(),
                }
            })
            .collect()
    }
}

impl Starter {
    /// 이 틀로 만들 새 초안 페이지의 입력. 블록 틀은 섹션 블록을 그리는 `blocks` 모양, 글 틀은 보통 페이지.
    pub fn page(&self, slug: &str, title: &str) -> NewPage {
        let (body_md, layout) = if self.body_md.is_empty() {
            (String::new(), "blocks")
        } else {
            (self.body_md.replace(TITLE_SLOT, title), "default")
        };
        NewPage {
            slug: slug.to_owned(),
            title: title.to_owned(),
            body_md,
            layout: layout.into(),
            sort_order: 0,
            parent: None,
        }
    }
}

/// 편집 캔버스가 그릴 페이지. 초안·숨김도 돌려준다(지운 것은 없음). 화면을 고칠 권한이 있어야 한다.
pub async fn editable_page(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Page> {
    actor.require(Permission::DesignEdit)?;
    let s = slug::normalize(slug).map_err(|_| not_found(slug))?;
    ctx.store
        .page_by_slug(&s)
        .await?
        .filter(|p| p.status != PageStatus::Deleted)
        .ok_or_else(|| not_found(slug))
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

#[cfg(test)]
mod nav_tests {
    use super::*;
    use crate::actor::AuthorType;

    fn page(id: &str, parent: Option<&str>, layout: &str) -> Page {
        Page {
            id: id.into(),
            slug: id.into(),
            title: id.to_uppercase(),
            body_md: String::new(),
            status: PageStatus::Published,
            layout: layout.into(),
            parent_id: parent.map(Into::into),
            author_id: None,
            actor_type: AuthorType::Agent,
            sort_order: 0,
            published_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            description: String::new(),
            share_image: String::new(),
            noindex: false,
        }
    }

    #[test]
    fn 문서는_위에서_아래로_읽는_순서로_앞뒤가_이어진다() {
        let all = [
            page("start", None, "docs"),
            page("install", Some("start"), "docs"),
            page("first", Some("start"), "docs"),
            page("guide", None, "docs"),
            page("about", None, "default"),
        ];
        let nav = navigation(&all, &all[2]);
        assert_eq!(nav.trail[0].slug, "start");
        assert_eq!(nav.prev.unwrap().slug, "install");
        assert_eq!(nav.next.unwrap().slug, "guide");
        assert_eq!(nav.tree.len(), 2, "문서가 아닌 페이지는 목록에 없다");
        assert_eq!(nav.tree[0].children.len(), 2);

        let top = navigation(&all, &all[0]);
        assert!(top.prev.is_none());
        assert_eq!(top.children.len(), 2);
    }

    #[test]
    fn 숨긴_위_페이지는_없는_셈_친다() {
        let all = [page("child", Some("hidden"), "docs")];
        let nav = navigation(&all, &all[0]);
        assert!(nav.trail.is_empty());
        assert_eq!(nav.tree.len(), 1);
    }

    #[test]
    fn 문서가_아닌_페이지는_목록을_만들지_않는다() {
        let all = [page("a", None, "default"), page("b", Some("a"), "default")];
        let nav = navigation(&all, &all[1]);
        assert_eq!(nav.trail.len(), 1);
        assert!(nav.tree.is_empty());
    }
}
