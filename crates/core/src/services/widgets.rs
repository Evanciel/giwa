//! 위젯 배치 (GAP S1, ADR-0059). 영역(`WIDGET_REGIONS`)과 화면 범위를 골라 놓는다.
//!
//! 코어 종류는 고정 목록이고, 모듈 종류는 `ctx.extensions` 로 묻는다 (ADR-0066). 종류마다 설정
//! 구조체가 있어 모르는 칸을 거부하고 기본값을 채운 모양(정규화)으로 저장한다. 화면 값(글 목록 등)은
//! web 이 만들고 테마가 그린다.
//! Phase 3 에서 쓰기가 changeset op 로 감싸진다 (ADR-0023).

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{MENU_LOCATIONS, WIDGET_REGIONS, Widget};
use crate::services::{menus, posts, sections};
use crate::slug;
use crate::store::{Ctx, Tx};

/// 사이트 전체 위젯 수 상한.
pub const MAX_WIDGETS: usize = 100;
const MAX_TITLE: usize = 60;
const MAX_MARKDOWN_BYTES: usize = 20_000;

/// 위젯 종류 하나의 설명. 에이전트가 `config_schema` 를 읽고 설정을 채운다.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WidgetType {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub config_schema: Value,
}

/// 최신 글. 게시판을 비우면 손님이 읽을 수 있는 모든 게시판.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LatestPostsConfig {
    /// 게시판 주소. 비우면 전체
    #[serde(default)]
    pub board: String,
    /// 1~20, 기본 5
    #[serde(default = "five")]
    pub limit: u32,
    /// `list`(제목 줄), `card`(사진 카드), `row`(옆으로 넘기는 카드 줄)
    #[serde(default = "list_view")]
    pub view: String,
    #[serde(default = "yes")]
    pub show_date: bool,
    /// 쪽 수 1~5, 기본 1. 2 이상이면 `limit` 개씩 「1/3」 쪽으로 넘긴다 (GAP Q23, ADR-0110)
    #[serde(default = "one")]
    pub pages: u32,
    /// 쪽을 저절로 넘긴다. 기본 끔. 멈춤 단추가 붙고, 움직임 줄이기 설정이면 넘기지 않는다
    #[serde(default)]
    pub auto: bool,
}

/// 인기 글: 최근 `days` 일 안에 발행된 공개 글을 조회수 → 댓글 수 → 최신 순으로.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PopularPostsConfig {
    #[serde(default)]
    pub board: String,
    #[serde(default = "five")]
    pub limit: u32,
    /// 0 이면 전체 기간, 최대 365. 기본 7
    #[serde(default = "seven")]
    pub days: u32,
    /// 모양: lines(제목 줄, 기본)·cards(그림 카드, 게시판 베스트) (GAP Q9, ADR-0094)
    #[serde(default = "lines")]
    pub style: String,
    /// 쪽 수 1~5, 기본 1. 2 이상이면 `limit` 개씩 「1/3」 쪽으로 넘긴다 (GAP Q23, ADR-0110)
    #[serde(default = "one")]
    pub pages: u32,
    /// 쪽을 저절로 넘긴다. 기본 끔. 멈춤 단추가 붙고, 움직임 줄이기 설정이면 넘기지 않는다
    #[serde(default)]
    pub auto: bool,
}

fn lines() -> String {
    "lines".into()
}

/// 인기 글 위젯 모양
pub const POPULAR_STYLES: [&str; 2] = ["lines", "cards"];

/// 글 탭의 탭 하나: 한 게시판(또는 전체)의 최신 글이나 인기 글.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostTab {
    /// 탭 이름, 1~20자. 한 위젯 안에서 겹치지 않는다
    pub label: String,
    /// `latest`(최신 글) 또는 `popular`(인기 글). 기본 latest
    #[serde(default = "latest_source")]
    pub source: String,
    /// 게시판 주소. 비우면 전체
    #[serde(default)]
    pub board: String,
    /// popular 일 때 기간(일). 0 이면 전체 기간, 최대 365. 기본 7
    #[serde(default = "seven")]
    pub days: u32,
}

/// 글 탭 (GAP Q3, ADR-0088): 여러 목록을 탭으로 묶은 상자. 저절로 넘어가지 않는다(접근성).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostTabsConfig {
    /// 2~6개
    pub tabs: Vec<PostTab>,
    /// 탭마다 글 수 1~20, 기본 5
    #[serde(default = "five")]
    pub limit: u32,
    #[serde(default = "yes")]
    pub show_date: bool,
    /// 탭마다 쪽 수 1~5, 기본 1. 2 이상이면 `limit` 개씩 「1/3」 쪽으로 넘긴다 (GAP Q29, ADR-0112)
    #[serde(default = "one")]
    pub pages: u32,
    /// 쪽을 저절로 넘긴다. 기본 끔. 탭은 저절로 바뀌지 않는다
    #[serde(default)]
    pub auto: bool,
}

pub const POST_TAB_SOURCES: [&str; 2] = ["latest", "popular"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkdownBlockConfig {
    /// Markdown 본문. 글 본문과 같은 규칙으로 정화해 그린다
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MenuConfig {
    /// `header` 또는 `footer` 메뉴
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoticeBarConfig {
    pub text: String,
    /// `/…` 또는 `https://…`. 비우면 링크 없음
    #[serde(default)]
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    #[serde(default)]
    pub placeholder: String,
}

/// 로그인 상자 (GAP Q15, ADR-0100). HTML 은 손님 모양으로 모두에게 같고, 로그인한 사람의 모양은
/// 테마 스크립트가 `/me/bar` 로 바꾼다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountBoxConfig {
    /// 손님에게 보일 한 줄 소개(200자). 비우면 사이트 설명
    #[serde(default)]
    pub intro: String,
    /// 아래 바로가기 단추(최대 6개). 출석체크·질문하기 같은 곳
    #[serde(default)]
    pub links: Vec<AccountBoxLink>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountBoxLink {
    pub label: String,
    /// `/…` 또는 `https://…`
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BannerSlide {
    /// 이 사이트에 올린 그림 주소(`/media/…`)
    pub image: String,
    /// 대체 글. 비우면 꾸밈 그림으로 본다
    #[serde(default)]
    pub alt: String,
    #[serde(default)]
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageBannerConfig {
    /// 1~8장. 여러 장이면 옆으로 넘기는 띠
    pub slides: Vec<BannerSlide>,
    /// 여러 장이면 저절로 넘긴다(회전 배너, GAP Q23). 기본 끔. 멈춤 단추가 붙는다
    #[serde(default)]
    pub auto: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GalleryGridConfig {
    pub board: String,
    /// 1~24, 기본 8
    #[serde(default = "eight")]
    pub limit: u32,
    /// 2~6, 기본 4 (좁은 화면은 2)
    #[serde(default = "four")]
    pub columns: u32,
}

/// 팝업(레이어, ADR-0060). `popup` 영역에만 놓인다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PopupConfig {
    /// Markdown 본문
    pub body: String,
    /// 이 사이트에 올린 그림(`/media/…`). 비우면 그림 없음
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub link: String,
    /// 이 시각부터 (RFC3339). 비우면 바로
    #[serde(default)]
    pub starts_at: String,
    /// 이 시각 전까지 (RFC3339). 비우면 끝없이
    #[serde(default)]
    pub ends_at: String,
    /// 「N일 동안 보지 않기」의 N. 0 이면 닫기 단추만. 0~30, 기본 1
    #[serde(default = "one")]
    pub dismiss_days: u32,
    /// `center`, `bottom-left`, `bottom-right`. 좁은 화면은 늘 아래쪽 판
    #[serde(default = "center")]
    pub position: String,
}

/// 팝업 위치.
pub const POPUP_POSITIONS: [&str; 3] = ["center", "bottom-left", "bottom-right"];

fn one() -> u32 {
    1
}
fn center() -> String {
    "center".into()
}
fn five() -> u32 {
    5
}
fn seven() -> u32 {
    7
}
fn eight() -> u32 {
    8
}
fn four() -> u32 {
    4
}
fn yes() -> bool {
    true
}
fn list_view() -> String {
    "list".into()
}
fn latest_source() -> String {
    "latest".into()
}

/// 종류 id, 이름, 설명.
const KINDS: [(&str, &str, &str); 11] = [
    (
        "latest_posts",
        "최신 글",
        "게시판(또는 전체)의 최근 글 목록",
    ),
    (
        "popular_posts",
        "인기 글",
        "최근 며칠 동안 올라온 글을 조회수 순으로",
    ),
    (
        "post_tabs",
        "글 탭",
        "게시판별 최신·인기 글을 탭으로 묶어 한 상자에",
    ),
    ("markdown_block", "글 상자", "Markdown 으로 쓴 안내·소개 글"),
    ("menu", "메뉴", "머리글·바닥글 메뉴를 한 번 더"),
    ("notice_bar", "알림 띠", "한 줄 알림과 링크"),
    ("search", "검색", "사이트 검색 칸"),
    (
        "account_box",
        "로그인 상자",
        "로그인·가입 단추와 바로가기. 로그인하면 내 이름·알림·포인트",
    ),
    (
        "image_banner",
        "그림 배너",
        "그림 한 장 또는 옆으로 넘기는 여러 장",
    ),
    (
        "gallery_grid",
        "사진 모음",
        "게시판 글의 대표 사진을 바둑판으로",
    ),
    (
        "popup",
        "팝업",
        "기간을 정해 띄우는 공지 창 (popup 영역 전용)",
    ),
];

pub(super) fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

pub use sections::is_section;

/// 코어가 가진 모든 종류: 위젯 + 섹션 블록 (ADR-0125)
fn all_kinds() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    KINDS.iter().chain(sections::KINDS.iter())
}

/// 놓을 수 있는 위젯 종류와 설정 모양.
pub fn types() -> Vec<WidgetType> {
    all_kinds()
        .map(|(id, name, description)| WidgetType {
            id,
            name,
            description,
            config_schema: match *id {
                id if sections::is_section(id) => sections::schema(id).unwrap_or(Value::Null),
                "latest_posts" => schema_of::<LatestPostsConfig>(),
                "popular_posts" => schema_of::<PopularPostsConfig>(),
                "post_tabs" => schema_of::<PostTabsConfig>(),
                "markdown_block" => schema_of::<MarkdownBlockConfig>(),
                "menu" => schema_of::<MenuConfig>(),
                "notice_bar" => schema_of::<NoticeBarConfig>(),
                "search" => schema_of::<SearchConfig>(),
                "account_box" => schema_of::<AccountBoxConfig>(),
                "image_banner" => schema_of::<ImageBannerConfig>(),
                "popup" => schema_of::<PopupConfig>(),
                _ => schema_of::<GalleryGridConfig>(),
            },
        })
        .collect()
}

/// 코어 종류에 켜진 모듈의 종류를 더한 목록 (ADR-0066).
pub async fn all_types(ctx: &Ctx) -> Vec<WidgetType> {
    let mut out = types();
    out.extend(ctx.extensions.widget_types(ctx).await);
    out
}

/// 한국어 표시 이름. 모르는 종류면 id 그대로.
pub fn kind_name(kind: &str) -> &str {
    all_kinds()
        .find(|(id, ..)| *id == kind)
        .map(|(_, name, _)| *name)
        .unwrap_or(kind)
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewWidget {
    pub kind: String,
    pub region: String,
    /// `all`(기본), `home`, `boards`, `board:<주소>`, `page:<주소>`
    #[serde(default = "all_scope")]
    pub scope: String,
    #[serde(default)]
    pub title: String,
    /// 종류마다 다른 설정 (`maru widget types`)
    #[serde(default)]
    pub config: Value,
    /// 영역 안 순서. 비우면 맨 뒤
    #[serde(default)]
    pub position: Option<i64>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn all_scope() -> String {
    "all".into()
}

/// 고칠 칸만. `config` 는 통째로 바꾼다.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WidgetPatch {
    pub region: Option<String>,
    pub scope: Option<String>,
    pub title: Option<String>,
    pub config: Option<Value>,
    pub position: Option<i64>,
    pub enabled: Option<bool>,
}

/// 위젯을 붙일 화면.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen<'a> {
    Home,
    Board(&'a str),
    Page(&'a str),
    /// 페이지를 첫 화면으로 쓸 때 (ADR-0125): `home` 과 `page:<주소>` 가 함께 붙는다
    HomePage(&'a str),
    /// 태그·검색·계정 등 나머지 공개 화면 (`all` 만 붙는다)
    Other,
}

impl Screen<'_> {
    /// 이 범위의 위젯을 이 화면에 붙이나.
    pub fn matches(&self, scope: &str) -> bool {
        match (scope, self) {
            ("all", _)
            | ("home", Screen::Home | Screen::HomePage(_))
            | ("boards", Screen::Board(_)) => true,
            _ => match (scope.split_once(':'), self) {
                (Some(("board", s)), Screen::Board(b)) => s == *b,
                (Some(("page", s)), Screen::Page(p) | Screen::HomePage(p)) => s == *p,
                _ => false,
            },
        }
    }
}

pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<Widget>> {
    actor.require(Permission::DesignEdit)?;
    ctx.store.list_widgets().await
}

pub async fn get(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Widget> {
    actor.require(Permission::DesignEdit)?;
    ctx.store
        .widget_by_id(id)
        .await?
        .ok_or_else(|| not_found(id))
}

/// 공개 화면에 붙일 켜진 위젯. 영역 → 순서. 권한을 보지 않는다(설정은 공개 화면에 그대로 드러나는 값이다).
/// 팝업은 기간 안(`starts_at ≤ 지금 < ends_at`)인 것만 (ADR-0060).
pub async fn for_screen(ctx: &Ctx, screen: Screen<'_>) -> Result<Vec<Widget>> {
    let now = clock::now();
    Ok(ctx
        .store
        .list_widgets()
        .await?
        .into_iter()
        .filter(|w| w.enabled && screen.matches(&w.scope) && in_period(w, &now))
        .collect())
}

/// 저장 형식(UTC, 같은 자릿수)이라 문자열 비교로 앞뒤를 안다.
fn in_period(w: &Widget, now: &str) -> bool {
    if w.kind != "popup" {
        return true;
    }
    let field = |k: &str| w.config.get(k).and_then(Value::as_str).unwrap_or("");
    let (starts, ends) = (field("starts_at"), field("ends_at"));
    (starts.is_empty() || starts <= now) && (ends.is_empty() || now < ends)
}

/// `popup` 종류와 `popup` 영역은 짝으로만 쓴다.
fn check_pair(kind: &str, region: &str) -> Result<()> {
    if (kind == "popup") == (region == "popup") {
        return Ok(());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.widget_popup_region")
        .arg("kind", kind)
        .arg("region", region))
}

pub async fn place(ctx: &Ctx, actor: &Actor, input: NewWidget) -> Result<Widget> {
    actor.require(Permission::DesignEdit)?;
    let kind = check_kind(ctx, &input.kind).await?;
    let region = check_region(&input.region)?;
    check_pair(&kind, &region)?;
    let scope = check_scope(&input.scope)?;
    let title = check_title(&input.title)?;
    let config = check_config(ctx, &kind, input.config).await?;

    let all = ctx.store.list_widgets().await?;
    // 같은 자리에 같은 설정이면 새로 놓지 않는다 (원칙 3: 여러 번 실행해도 안전).
    if let Some(same) = all.iter().find(|w| {
        w.kind == kind
            && w.region == region
            && w.scope == scope
            && w.title == title
            && w.config == config
    }) {
        return Ok(same.clone());
    }
    if all.len() >= MAX_WIDGETS {
        return Err(Error::new(Code::Validation)
            .with_message("error.widget_too_many")
            .arg("max", MAX_WIDGETS.to_string()));
    }
    let sort_order = input.position.unwrap_or_else(|| {
        all.iter()
            .filter(|w| w.region == region)
            .map(|w| w.sort_order + 1)
            .max()
            .unwrap_or(0)
    });
    let now = clock::now();
    let widget = Widget {
        id: clock::new_id(),
        kind,
        region,
        scope,
        title,
        config,
        sort_order,
        enabled: input.enabled,
        created_at: now.clone(),
        updated_at: now,
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.insert_widget(&widget).await?;
    tx.audit(
        actor,
        "widget.place",
        Target::new("widget", &widget.id),
        json!({ "after": widget }),
    )
    .await?;
    tx.commit().await?;
    Ok(widget)
}

pub async fn configure(ctx: &Ctx, actor: &Actor, id: &str, patch: WidgetPatch) -> Result<Widget> {
    actor.require(Permission::DesignEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx.widget_by_id(id).await?.ok_or_else(|| not_found(id))?;
    let mut after = before.clone();
    if let Some(r) = &patch.region {
        after.region = check_region(r)?;
        check_pair(&after.kind, &after.region)?;
    }
    if let Some(s) = &patch.scope {
        after.scope = check_scope(s)?;
    }
    if let Some(t) = &patch.title {
        after.title = check_title(t)?;
    }
    if let Some(c) = patch.config {
        after.config = check_config(ctx, &after.kind, c).await?;
    }
    if let Some(p) = patch.position {
        after.sort_order = p;
    }
    if let Some(e) = patch.enabled {
        after.enabled = e;
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    tx.update_widget(&after).await?;
    tx.audit(
        actor,
        "widget.configure",
        Target::new("widget", id),
        json!({ "before": before, "after": after }),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn remove(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Widget> {
    actor.require(Permission::DesignEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx.widget_by_id(id).await?.ok_or_else(|| not_found(id))?;
    tx.delete_widget(id).await?;
    tx.audit(
        actor,
        "widget.remove",
        Target::new("widget", id),
        json!({ "before": before }),
    )
    .await?;
    tx.commit().await?;
    Ok(before)
}

fn not_found(id: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.widget_not_found")
        .arg("id", id)
        .next("maru widget list")
}

async fn check_kind(ctx: &Ctx, kind: &str) -> Result<String> {
    let k = kind.trim();
    let extra = ctx.extensions.widget_types(ctx).await;
    if all_kinds().any(|(id, ..)| *id == k) || extra.iter().any(|t| t.id == k) {
        return Ok(k.to_owned());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.widget_kind")
        .arg("value", kind)
        .arg(
            "allowed",
            all_kinds()
                .map(|(id, ..)| *id)
                .chain(extra.iter().map(|t| t.id))
                .collect::<Vec<_>>()
                .join(", "),
        )
        .next("maru widget types"))
}

fn check_region(region: &str) -> Result<String> {
    let r = region.trim();
    if WIDGET_REGIONS.contains(&r) {
        return Ok(r.to_owned());
    }
    Err(Error::new(Code::Validation)
        .with_message("error.widget_region")
        .arg("value", region)
        .arg("allowed", WIDGET_REGIONS.join(", ")))
}

/// 범위를 저장 형태로. 주소는 slug 규칙으로 맞춘다(게시판·페이지가 아직 없어도 된다).
pub fn check_scope(scope: &str) -> Result<String> {
    let s = scope.trim();
    if matches!(s, "all" | "home" | "boards") {
        return Ok(s.to_owned());
    }
    let bad = || {
        Error::new(Code::Validation)
            .with_message("error.widget_scope")
            .arg("value", scope)
    };
    match s.split_once(':') {
        Some((kind @ ("board" | "page"), rest)) => {
            let rest = slug::normalize(rest).map_err(|_| bad())?;
            Ok(format!("{kind}:{rest}"))
        }
        _ => Err(bad()),
    }
}

fn check_title(title: &str) -> Result<String> {
    let t = title.trim();
    if t.chars().count() > MAX_TITLE {
        return Err(Error::new(Code::Validation)
            .with_message("error.field_length")
            .arg("field", "title")
            .arg("max", MAX_TITLE.to_string()));
    }
    Ok(t.to_owned())
}

pub(super) fn bad_config(kind: &str, reason: impl Into<String>) -> Error {
    Error::new(Code::Validation)
        .with_message("error.widget_config")
        .arg("kind", kind)
        .arg("reason", reason.into())
        .next(format!(
            "maru widget types --json  # {kind} 의 config_schema"
        ))
}

pub(super) fn parse<T: DeserializeOwned>(kind: &str, config: Value) -> Result<T> {
    let config = if config.is_null() { json!({}) } else { config };
    serde_json::from_value(config).map_err(|e| bad_config(kind, e.to_string()))
}

fn in_range(kind: &str, field: &str, v: u32, lo: u32, hi: u32) -> Result<()> {
    if (lo..=hi).contains(&v) {
        return Ok(());
    }
    Err(bad_config(kind, format!("{field} 는 {lo}~{hi}")))
}

/// 게시판 주소를 맞추고 있는지 본다. 빈 값은 `allow_empty` 일 때만.
async fn check_board(ctx: &Ctx, kind: &str, board: &str, allow_empty: bool) -> Result<String> {
    let b = board.trim();
    if b.is_empty() {
        return if allow_empty {
            Ok(String::new())
        } else {
            Err(bad_config(kind, "board 가 필요합니다"))
        };
    }
    let s = slug::normalize(b)?;
    if ctx.store.board_by_slug(&s).await?.is_none() {
        return Err(Error::new(Code::NotFound)
            .with_message("error.board_not_found")
            .arg("slug", s)
            .next("maru board list"));
    }
    Ok(s)
}

pub(super) fn check_link(kind: &str, link: &str) -> Result<String> {
    let l = link.trim();
    if l.is_empty() {
        return Ok(String::new());
    }
    menus::check_href(l)
        .map_err(|_| bad_config(kind, format!("link 주소가 올바르지 않습니다: {l}")))
}

/// RFC3339 시각을 저장 형식(UTC)으로. 빈 값은 그대로.
fn check_time(kind: &str, field: &str, v: &str) -> Result<String> {
    let t = v.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    clock::parse(t).map(clock::format).ok_or_else(|| {
        bad_config(
            kind,
            format!("{field} 는 RFC3339 시각 (예: 2026-11-11T09:00:00+09:00)"),
        )
    })
}

pub(super) fn short_text(
    kind: &str,
    field: &str,
    v: &str,
    max: usize,
    required: bool,
) -> Result<String> {
    let t = v.trim();
    if (required && t.is_empty()) || t.chars().count() > max {
        let need = if required { "1" } else { "0" };
        return Err(bad_config(kind, format!("{field} 는 {need}~{max}자")));
    }
    Ok(t.to_owned())
}

/// 종류에 맞는 설정인지 보고 기본값을 채운 모양으로 돌려준다.
async fn check_config(ctx: &Ctx, kind: &str, config: Value) -> Result<Value> {
    if sections::is_section(kind) {
        return sections::check(kind, config).unwrap_or(Ok(Value::Null));
    }
    // 코어 목록에 없으면 모듈 몫이다. 모듈이 꺼져 있으면 모르는 종류로 친다.
    if !KINDS.iter().any(|(id, ..)| *id == kind) {
        // 설정을 안 적었으면 빈 값으로 넘겨 모듈이 기본값을 채우게 한다.
        let config = if config.is_null() { json!({}) } else { config };
        return match ctx.extensions.check_widget(ctx, kind, config).await {
            Some(r) => r,
            None => check_kind(ctx, kind).await.map(|_| Value::Null),
        };
    }
    let v = match kind {
        "latest_posts" => {
            let mut c: LatestPostsConfig = parse(kind, config)?;
            in_range(kind, "limit", c.limit, 1, 20)?;
            in_range(kind, "pages", c.pages, 1, 5)?;
            if !["list", "card", "row"].contains(&c.view.as_str()) {
                return Err(bad_config(kind, "view 는 list·card·row"));
            }
            c.board = check_board(ctx, kind, &c.board, true).await?;
            json!(c)
        }
        "popular_posts" => {
            let mut c: PopularPostsConfig = parse(kind, config)?;
            in_range(kind, "limit", c.limit, 1, 20)?;
            in_range(kind, "pages", c.pages, 1, 5)?;
            in_range(kind, "days", c.days, 0, 365)?;
            if !POPULAR_STYLES.contains(&c.style.as_str()) {
                return Err(bad_config(kind, "style 은 lines·cards"));
            }
            c.board = check_board(ctx, kind, &c.board, true).await?;
            json!(c)
        }
        "post_tabs" => {
            let mut c: PostTabsConfig = parse(kind, config)?;
            in_range(kind, "limit", c.limit, 1, 20)?;
            in_range(kind, "pages", c.pages, 1, 5)?;
            if !(2..=6).contains(&c.tabs.len()) {
                return Err(bad_config(kind, "tabs 는 2~6개"));
            }
            let mut seen: Vec<String> = Vec::new();
            for t in &mut c.tabs {
                t.label = short_text(kind, "label", &t.label, 20, true)?;
                if seen.contains(&t.label) {
                    return Err(bad_config(
                        kind,
                        format!("탭 이름 `{}` 이 겹칩니다", t.label),
                    ));
                }
                seen.push(t.label.clone());
                t.source = t.source.trim().to_owned();
                if !POST_TAB_SOURCES.contains(&t.source.as_str()) {
                    return Err(bad_config(kind, "source 는 latest 또는 popular"));
                }
                in_range(kind, "days", t.days, 0, 365)?;
                t.board = check_board(ctx, kind, &t.board, true).await?;
            }
            json!(c)
        }
        "markdown_block" => {
            let mut c: MarkdownBlockConfig = parse(kind, config)?;
            c.body = super::body_text(c.body, MAX_MARKDOWN_BYTES)?;
            if c.body.trim().is_empty() {
                return Err(bad_config(kind, "body 가 비었습니다"));
            }
            json!(c)
        }
        "menu" => {
            let mut c: MenuConfig = parse(kind, config)?;
            c.location = c.location.trim().to_owned();
            if !MENU_LOCATIONS.contains(&c.location.as_str()) {
                return Err(bad_config(
                    kind,
                    format!("location 은 {}", MENU_LOCATIONS.join(", ")),
                ));
            }
            json!(c)
        }
        "notice_bar" => {
            let mut c: NoticeBarConfig = parse(kind, config)?;
            c.text = short_text(kind, "text", &c.text, 200, true)?;
            c.link = check_link(kind, &c.link)?;
            json!(c)
        }
        "search" => {
            let mut c: SearchConfig = parse(kind, config)?;
            c.placeholder = short_text(kind, "placeholder", &c.placeholder, 40, false)?;
            json!(c)
        }
        "account_box" => {
            let mut c: AccountBoxConfig = parse(kind, config)?;
            c.intro = short_text(kind, "intro", &c.intro, 200, false)?;
            if c.links.len() > 6 {
                return Err(bad_config(kind, "links 는 6개까지"));
            }
            for l in &mut c.links {
                l.label = short_text(kind, "links.label", &l.label, 20, true)?;
                l.link = check_link(kind, &l.link)?;
                if l.link.is_empty() {
                    return Err(bad_config(kind, "links.link 가 비었습니다"));
                }
            }
            json!(c)
        }
        "image_banner" => {
            let mut c: ImageBannerConfig = parse(kind, config)?;
            if c.slides.is_empty() || c.slides.len() > 8 {
                return Err(bad_config(kind, "slides 는 1~8장"));
            }
            for s in &mut c.slides {
                s.image = posts::check_cover(&s.image)?
                    .ok_or_else(|| bad_config(kind, "slides.image 가 비었습니다"))?;
                s.alt = short_text(kind, "slides.alt", &s.alt, 120, false)?;
                s.link = check_link(kind, &s.link)?;
            }
            json!(c)
        }
        "popup" => {
            let mut c: PopupConfig = parse(kind, config)?;
            c.body = super::body_text(c.body, MAX_MARKDOWN_BYTES)?;
            if c.body.trim().is_empty() {
                return Err(bad_config(kind, "body 가 비었습니다"));
            }
            c.image = posts::check_cover(&c.image)?.unwrap_or_default();
            c.link = check_link(kind, &c.link)?;
            c.starts_at = check_time(kind, "starts_at", &c.starts_at)?;
            c.ends_at = check_time(kind, "ends_at", &c.ends_at)?;
            if !c.starts_at.is_empty() && !c.ends_at.is_empty() && c.starts_at >= c.ends_at {
                return Err(bad_config(kind, "ends_at 은 starts_at 보다 뒤여야 합니다"));
            }
            in_range(kind, "dismiss_days", c.dismiss_days, 0, 30)?;
            c.position = c.position.trim().to_owned();
            if !POPUP_POSITIONS.contains(&c.position.as_str()) {
                return Err(bad_config(
                    kind,
                    format!("position 은 {}", POPUP_POSITIONS.join(", ")),
                ));
            }
            json!(c)
        }
        _ => {
            let mut c: GalleryGridConfig = parse(kind, config)?;
            in_range(kind, "limit", c.limit, 1, 24)?;
            in_range(kind, "columns", c.columns, 2, 6)?;
            c.board = check_board(ctx, kind, &c.board, false).await?;
            json!(c)
        }
    };
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_match_screens() {
        assert!(Screen::Home.matches("all"));
        assert!(Screen::Home.matches("home"));
        assert!(!Screen::Home.matches("boards"));
        assert!(Screen::Board("free").matches("boards"));
        assert!(Screen::Board("free").matches("board:free"));
        assert!(!Screen::Board("free").matches("board:qna"));
        assert!(!Screen::Board("free").matches("page:free"));
        assert!(Screen::Page("about").matches("page:about"));
        assert!(Screen::HomePage("about").matches("page:about"));
        assert!(Screen::HomePage("about").matches("home"));
        assert!(!Screen::Page("about").matches("home"));
        assert!(!Screen::Other.matches("home"));
        assert!(Screen::Other.matches("all"));
    }

    #[test]
    fn scopes_are_checked() {
        assert_eq!(check_scope(" home ").unwrap(), "home");
        assert_eq!(check_scope("board:Free").unwrap(), "board:free");
        for bad in ["", "everywhere", "board:", "post:x", "board:../x"] {
            assert!(check_scope(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_kind_has_a_schema() {
        let t = types();
        assert_eq!(t.len(), KINDS.len() + sections::KINDS.len());
        assert!(t.iter().all(|w| w.config_schema.is_object()));
    }
}
