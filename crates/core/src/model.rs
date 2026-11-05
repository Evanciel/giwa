//! 도메인 모델. 저장소는 이 타입으로 읽고 쓴다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{AuthorType, Permission, Scope};
use crate::clock::Id;

/// 게시판 종류 (SPEC §4). DB 에 CHECK 를 두지 않고 여기서 검증한다 (ADR-0016).
pub const BOARD_KINDS: [&str; 8] = [
    "blog", "notice", "gallery", "free", "qna", "faq", "forum", "wiki",
];
/// 게시판 목록 순서 (ADR-0058). 빈 값이면 종류의 기본값.
pub const BOARD_ORDERS: [&str; 2] = ["latest", "activity"];
/// 댓글을 채택할 수 있는 게시판 종류 (ADR-0058).
pub const ACCEPT_KINDS: [&str; 2] = ["qna", "forum"];

/// 게시판 목록 형태 (ADR-0054). 테마가 이름마다 모양을 그린다.
pub const BOARD_LAYOUTS: [&str; 4] = ["list", "webzine", "card", "gallery"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BoardPolicy {
    /// 목록을 볼 수 있는 최소 역할
    #[serde(default = "guest")]
    pub list: String,
    /// 글을 읽을 수 있는 최소 역할
    #[serde(default = "guest")]
    pub read: String,
    /// 글을 쓸 수 있는 최소 역할
    #[serde(default = "member")]
    pub write: String,
    /// 남의 글을 관리할 수 있는 최소 역할
    #[serde(default = "editor")]
    pub moderate: String,
    /// 댓글을 쓸 수 있는 최소 역할. `none` 이면 댓글을 받지 않는다 (ADR-0052)
    #[serde(default = "member")]
    pub comment: String,
    /// 쓴 댓글을 바로 보일지, 관리자 승인 뒤 보일지
    #[serde(default)]
    pub comment_approval: Approval,
    /// 공개 화면에서 쓴 글을 바로 보일지, 관리자 승인 뒤 보일지 (ADR-0053)
    #[serde(default)]
    pub write_approval: Approval,
    /// 답변글을 달 수 있는 최소 역할. `none` 이면 받지 않는다 (ADR-0056)
    #[serde(default = "no_role")]
    pub reply: String,
    /// 공개 글쓰기 화면에서 파일을 붙일 수 있는 최소 역할. `none` 이면 받지 않는다.
    /// 손님은 고를 수 없다 (ADR-0065)
    #[serde(default = "no_role")]
    pub upload: String,
}

/// 댓글·공개 글쓰기 승인 (게시판 정책, ADR-0052·0053).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    /// 바로 보인다
    Off,
    /// 비회원 댓글만 승인 뒤 보인다
    #[default]
    Guest,
    /// 모든 댓글이 승인 뒤 보인다 (관리 권한이 있는 사람 것은 바로)
    All,
}

impl Approval {
    pub const ALL: [Self; 3] = [Self::Off, Self::Guest, Self::All];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Guest => "guest",
            Self::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

fn guest() -> String {
    "guest".into()
}
fn member() -> String {
    "member".into()
}
fn editor() -> String {
    "editor".into()
}
fn no_role() -> String {
    "none".into()
}

impl Default for BoardPolicy {
    fn default() -> Self {
        Self {
            list: guest(),
            read: guest(),
            write: member(),
            moderate: editor(),
            comment: member(),
            comment_approval: Approval::Guest,
            write_approval: Approval::Guest,
            reply: no_role(),
            upload: no_role(),
        }
    }
}

impl BoardPolicy {
    /// 블로그: 누구나 읽고, 글은 편집자 이상만 쓴다.
    pub fn blog() -> Self {
        Self {
            write: editor(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Board {
    pub id: Id,
    pub slug: String,
    pub name: String,
    pub kind: String,
    /// 목록 형태(`BOARD_LAYOUTS`). 빈 값이면 종류의 기본값 (ADR-0054)
    #[serde(default)]
    pub layout: String,
    /// 목록 순서(`BOARD_ORDERS`). 빈 값이면 종류의 기본값 (ADR-0058)
    #[serde(default)]
    pub list_order: String,
    pub policy: BoardPolicy,
    pub sort_order: i64,
    /// 글에 붙일 여분 칸 정의 (ADR-0057)
    #[serde(default)]
    pub field_schema: Vec<crate::fields::FieldDef>,
    /// 목록 위 안내 문구, 평문 (GAP Q9, ADR-0094). 빈 값이면 보이지 않는다
    #[serde(default)]
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Post {
    /// 답변 깊이. 맨 위 글은 0.
    pub fn depth(&self) -> usize {
        self.thread_path.len() / 5
    }
}

impl Board {
    /// 실제로 쓸 목록 형태. 따로 정하지 않았으면 종류로 고른다.
    pub fn effective_layout(&self) -> &str {
        if !self.layout.is_empty() {
            return &self.layout;
        }
        match self.kind.as_str() {
            "blog" => "webzine",
            "gallery" => "gallery",
            _ => "list",
        }
    }

    /// 실제로 쓸 목록 순서. 따로 정하지 않았으면 `forum` 만 활동순.
    pub fn effective_order(&self) -> &str {
        if !self.list_order.is_empty() {
            return &self.list_order;
        }
        if self.kind == "forum" {
            "activity"
        } else {
            "latest"
        }
    }

    /// 댓글을 채택할 수 있는 게시판인가.
    pub fn accepts_answers(&self) -> bool {
        ACCEPT_KINDS.contains(&self.kind.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostStatus {
    Draft,
    Review,
    Published,
    Hidden,
    Pending,
    Spam,
    Deleted,
}

impl PostStatus {
    pub const ALL: [PostStatus; 7] = [
        Self::Draft,
        Self::Review,
        Self::Published,
        Self::Hidden,
        Self::Pending,
        Self::Spam,
        Self::Deleted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Review => "review",
            Self::Published => "published",
            Self::Hidden => "hidden",
            Self::Pending => "pending",
            Self::Spam => "spam",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostKind {
    Post,
    Comment,
}

impl PostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Comment => "comment",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "post" => Some(Self::Post),
            "comment" => Some(Self::Comment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Post {
    pub id: Id,
    pub board_id: Id,
    pub kind: PostKind,
    pub status: PostStatus,
    pub title: String,
    pub slug: String,
    pub body_md: String,
    pub excerpt: Option<String>,
    pub author_id: Option<Id>,
    pub actor_type: AuthorType,
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// 이 시각이 지나면 서버가 발행한다. 공개된 글에는 없다.
    #[serde(default)]
    pub scheduled_at: Option<String>,
    /// 대표 이미지 주소 (`/media/...`). 없으면 본문 첫 사진을 쓴다.
    #[serde(default)]
    pub cover: Option<String>,
    /// 게시판 안의 분류. 분류를 지우면 비워진다.
    #[serde(default)]
    pub category_id: Option<Id>,
    /// 게시판 목록 맨 위에 고정한다.
    #[serde(default)]
    pub is_notice: bool,
    /// 목록에는 제목만 보이고, 본문은 작성자와 관리자만 읽는다.
    #[serde(default)]
    pub is_secret: bool,
    /// 태그 이름 (글쓴이가 적은 순서).
    #[serde(default)]
    pub tags: Vec<String>,
    /// 조회수. 서버가 모아 두었다가 주기마다 더하므로 몇십 초 늦을 수 있다 (ADR-0049).
    #[serde(default)]
    pub view_count: i64,
    /// 비회원 글이면 적은 이름 (ADR-0053). 이때 `author_id` 는 비고 `actor_type` 은 `human`.
    #[serde(default)]
    pub guest_name: Option<String>,
    /// 비회원 비밀번호의 argon2id 해시. 밖으로 내보내지 않는다.
    #[serde(skip)]
    pub guest_pw_hash: Option<String>,
    /// 답변글이면 바로 위 글 (ADR-0056)
    #[serde(default)]
    pub parent_id: Option<Id>,
    /// 답변글이면 맨 위 글. 맨 위 글 자신은 비어 있다
    #[serde(default)]
    pub thread_id: Option<Id>,
    /// 스레드 안 순서 `0001.0002.` (깊이마다 다섯 글자). 맨 위 글은 빈 값
    #[serde(default)]
    pub thread_path: String,
    /// 맨 위 글을 쓴 회원. 비밀 답변을 질문한 사람도 읽게 한다
    #[serde(default)]
    pub thread_owner: Option<Id>,
    /// 여분 칸 값 (게시판 `field_schema` 의 키 → 값, ADR-0057)
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
    /// 마지막 공개 댓글·답변 시각. 없으면 발행 시각을 쓴다 (ADR-0058)
    #[serde(default)]
    pub last_activity_at: Option<String>,
    /// 채택한 댓글 (`qna`·`forum`, ADR-0058)
    #[serde(default)]
    pub accepted_comment_id: Option<Id>,
    /// 잠긴 글: 관리자 말고는 댓글·답변·고치기·지우기를 못 한다 (ADR-0058)
    #[serde(default)]
    pub is_locked: bool,
}

impl Post {
    /// 공개 여부 판단은 이것 하나다. 게시판 읽기 권한은 별도로 본다.
    /// 비밀글도 공개 글이다(목록에 보인다). 본문을 보여 줄지는 `services::posts` 가 정한다.
    pub fn is_public(&self) -> bool {
        self.status == PostStatus::Published && self.kind == PostKind::Post
    }
}

/// 게시판 안의 분류.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Category {
    pub id: Id,
    pub board_id: Id,
    pub name: String,
    pub slug: String,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// 사이트 전체 태그. `posts` 는 공개 글 수다(목록 조회 때만 채운다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Tag {
    pub id: Id,
    pub name: String,
    pub slug: String,
    pub created_at: String,
    #[serde(default)]
    pub posts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Revision {
    pub id: Id,
    pub post_id: Option<Id>,
    pub page_id: Option<Id>,
    pub title: String,
    pub body_md: String,
    pub actor_type: AuthorType,
    pub actor_id: Option<Id>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub id: Id,
    pub name: String,
    pub rank: i64,
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Token {
    pub id: Id,
    pub name: String,
    #[serde(skip)]
    pub token_hash: String,
    pub scopes: Vec<Scope>,
    pub board_ids: Option<Vec<Id>>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 업로드한 이미지. 원본은 다시 인코딩해 메타데이터(EXIF 위치 등)를 지운 것이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Media {
    pub id: Id,
    /// 보관소 상대 경로
    pub path: String,
    /// 사이트 안 주소 (`/media/...`)
    pub url: String,
    pub mime: String,
    pub size: i64,
    pub width: u32,
    pub height: u32,
    pub alt_text: Option<String>,
    /// 폭을 줄인 사본. 원본보다 좁은 것만 있다.
    pub variants: Vec<MediaVariant>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MediaVariant {
    pub width: u32,
    pub height: u32,
    pub path: String,
    /// 읽을 때 경로로 채운다. 저장하지 않는다.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    pub size: i64,
}

/// 첨부 파일. 그림과 달리 다시 인코딩하지 않고, 내려받기(`Content-Disposition: attachment`)로만 내보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Attachment {
    pub id: Id,
    /// 보관소 상대 경로 (`files/2026/11/<id>.pdf`)
    pub path: String,
    /// 내려받는 주소 (`/files/<id>`)
    pub url: String,
    /// 올릴 때의 파일 이름. 화면과 내려받기 이름에만 쓴다
    pub filename: String,
    pub mime: String,
    pub size: i64,
    pub created_at: String,
}

/// 운영자가 올린 서체 (ADR-0048). 글 안 `:font[…]{slug}` 와 토큰 `font.body`·`font.heading` 에 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Font {
    pub id: Id,
    /// `ff-{slug}` class 와 토큰 값 (소문자·숫자·`-`)
    pub slug: String,
    /// 메뉴·관리 화면에 보일 이름
    pub name: String,
    /// 파일을 못 받았을 때 떨어질 계열: `sans` | `serif` | `mono` | `cursive`
    pub fallback: String,
    /// `OFL-1.1` | `Apache-2.0` | `MIT` | `CC0-1.0` | `other`
    pub license: String,
    /// 붙인 라이선스 전문의 보관소 경로
    pub license_path: Option<String>,
    /// 라이선스 전문 주소 (`/fonts/{slug}/LICENSE.txt`). 전문이 없으면 비어 있다
    pub license_url: Option<String>,
    /// 굵기·기울임 순
    pub files: Vec<FontFile>,
    pub created_at: String,
    pub updated_at: String,
}

/// 서체의 굵기·기울임 하나에 해당하는 파일.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FontFile {
    pub id: Id,
    /// 100~900
    pub weight: i64,
    pub italic: bool,
    /// 첫 바이트로 판정한 형식: `woff2` | `woff` | `ttf` | `otf`
    pub format: String,
    /// 보관소 상대 경로 (`fonts/{slug}/{sha256 앞 16자}.{format}`)
    pub path: String,
    /// 공개 주소 (`/fonts/{slug}/{sha256 앞 16자}.{format}`)
    pub url: String,
    pub size: i64,
    pub sha256: String,
    pub created_at: String,
}

/// 글 틀. 새 글 본문으로 불러오는 뼈대.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PostTemplate {
    pub id: Id,
    pub name: String,
    pub body_md: String,
    /// 바이너리에 든 기본 틀. 고치거나 지울 수 없다
    pub builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 목록 조회의 쪽 단위.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PageReq {
    pub page: u32,
    pub per_page: u32,
}

impl PageReq {
    pub const MAX_PER_PAGE: u32 = 100;

    pub fn new(page: u32, per_page: u32) -> Self {
        Self {
            page: page.max(1),
            per_page: per_page.clamp(1, Self::MAX_PER_PAGE),
        }
    }

    pub fn limit(&self) -> i64 {
        i64::from(self.per_page)
    }

    pub fn offset(&self) -> i64 {
        i64::from(self.page - 1) * i64::from(self.per_page)
    }
}

impl Default for PageReq {
    fn default() -> Self {
        Self::new(1, 10)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

impl<T> Listing<T> {
    pub fn total_pages(&self) -> i64 {
        let per = i64::from(self.per_page.max(1));
        (self.total + per - 1) / per
    }
}

/// 페이지 레이아웃 (ADR-0016: CHECK 없이 여기서 검증).
pub const PAGE_LAYOUTS: [&str; 2] = ["default", "wide"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    Draft,
    Published,
    Hidden,
    Deleted,
}

impl PageStatus {
    pub const ALL: [PageStatus; 4] = [Self::Draft, Self::Published, Self::Hidden, Self::Deleted];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Hidden => "hidden",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Page {
    pub id: Id,
    pub slug: String,
    pub title: String,
    pub body_md: String,
    pub status: PageStatus,
    pub layout: String,
    pub author_id: Option<Id>,
    pub actor_type: AuthorType,
    pub sort_order: i64,
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 메뉴 위치. `quick` 은 검색 칸 옆 바로가기 단추 줄이다 (GAP Q12, ADR-0097).
pub const MENU_LOCATIONS: [&str; 3] = ["header", "footer", "quick"];

/// 메뉴 항목이 가리키는 곳. 주소는 렌더할 때 `urls` 로 만든다 (ADR-0017).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuLink {
    Home,
    Board {
        slug: String,
    },
    Page {
        slug: String,
    },
    /// `https://`, `http://` 또는 `/` 로 시작하는 사이트 안 경로
    Url {
        href: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MenuItem {
    pub label: String,
    pub link: MenuLink,
    #[serde(default)]
    pub children: Vec<MenuItem>,
}

/// 위젯을 놓는 영역 (SPEC §8.3, ADR-0059). 테마가 그리지 않는 영역의 위젯은 빠진다.
/// `popup` 은 팝업 종류 전용 자리다 (ADR-0060). `masthead` 는 머리글의 사이트 이름 옆(광고·배너)이다 (ADR-0097).
pub const WIDGET_REGIONS: [&str; 7] = [
    "header", "hero", "main", "sidebar", "footer", "popup", "masthead",
];

/// 감사 기록 한 줄 (GAP S14, ADR-0062). `actor_name` 은 회원 표시 이름·토큰 이름(없으면 빈 값).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEntry {
    pub id: String,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub actor_name: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub diff: serde_json::Value,
    pub at: String,
}

/// 모듈 상태 한 줄 (ADR-0066). 줄이 있으면 설치된 것이다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModuleRow {
    pub id: String,
    pub enabled: bool,
    pub version: String,
    /// 기본값과 달리 저장한 설정
    pub settings: serde_json::Value,
    /// 이 모듈이 여기까지 받은 감사 기록 순번
    pub cursor: i64,
    pub installed_at: String,
    pub updated_at: String,
}

/// 모듈이 처리하지 못한 이벤트 (ADR-0066).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModuleFailure {
    pub module: String,
    pub event_id: String,
    pub action: String,
    pub attempts: i64,
    pub last_error: String,
    pub next_at: String,
    /// 재시도를 멈췄다
    pub gave_up: bool,
    pub first_at: String,
}

/// 모듈에 넘기는 이벤트: 감사 기록 한 줄 (ADR-0066). `seq` 는 적힌 순서.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEvent {
    pub seq: i64,
    pub id: String,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub diff: serde_json::Value,
    pub at: String,
}

/// 하루 방문 집계 (ADR-0061). `day` 는 한국 시간 `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatDay {
    pub day: String,
    pub views: i64,
    pub visitors: i64,
}

/// 화면 하나의 집계. `title` 은 요약할 때 글·페이지·게시판 이름을 붙인 것(모르면 빈 값).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatPage {
    pub path: String,
    #[serde(default)]
    pub title: String,
    pub views: i64,
    pub visitors: i64,
}

/// 들어온 곳(Referer 호스트) 하나의 집계.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatReferrer {
    pub host: String,
    pub visits: i64,
}

/// 한 번에 더할 통계. 화면·들어온 곳은 (날, 값).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatBatch {
    pub days: Vec<StatDay>,
    pub pages: Vec<(String, StatPage)>,
    pub referrers: Vec<(String, StatReferrer)>,
}

/// 영역에 놓인 위젯 하나. `config` 는 종류마다 정해진 모양으로 core 가 검사·정규화한 값이다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Widget {
    pub id: Id,
    pub kind: String,
    pub region: String,
    /// `all`, `home`, `boards`, `board:<주소>`, `page:<주소>`
    pub scope: String,
    pub title: String,
    pub config: serde_json::Value,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 사이트 설정 (`site_settings`). 키 하나에 값 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SiteSettings {
    pub name: String,
    pub description: String,
    /// BCP 47 언어 태그
    pub lang: String,
    /// UTC 기준 분 (+540 = 한국)
    pub tz_offset_min: i32,
    pub posts_per_page: u32,
    /// 네이버 서치어드바이저 소유 확인 값 (`naver-site-verification` meta)
    #[serde(default)]
    pub naver_verification: String,
    /// 구글 서치 콘솔 소유 확인 값
    #[serde(default)]
    pub google_verification: String,
    /// 글·페이지에 h2~h4 제목이 이 개수 이상이면 본문 위에 목차를 보인다. 0 이면 끔
    #[serde(default = "default_toc_min_headings")]
    pub toc_min_headings: u32,
    /// 공개 화면(글·게시판 목록)에 조회수를 보인다. 꺼도 세기는 계속한다
    #[serde(default = "default_true")]
    pub show_views: bool,
    /// 공개 화면 머리글에 사이트 검색 칸을 보인다 (`/search` 는 꺼도 열린다)
    #[serde(default = "default_true")]
    pub show_search: bool,
    /// 회원 가입: off(받지 않음) · open(누구나). 꺼도 이미 있는 회원은 로그인한다
    #[serde(default)]
    pub signup: SignupMode,
    /// 가입한 사람이 메일 링크를 눌러야 로그인할 수 있다
    #[serde(default = "default_true")]
    pub signup_verify: bool,
    /// 사람 확인 질문을 누구에게 낼지 (GAP M10, ADR-0052)
    #[serde(default)]
    pub captcha: CaptchaMode,
    /// 방문자 통계를 며칠 남길지 (30~3650, ADR-0061)
    #[serde(default = "default_stats_retention_days")]
    pub stats_retention_days: u32,
    /// 공개 글·댓글에 쓸 수 없는 말 (ADR-0071)
    #[serde(default)]
    pub banned_words: Vec<String>,
    /// 회원이 글·댓글을 쓰는 최소 간격(초). 0 이면 끔 (ADR-0071)
    #[serde(default)]
    pub flood_seconds: u32,
    /// 이 날수 동안 로그인하지 않은 회원을 휴면으로 바꾼다. 0 이면 끔 (ADR-0073)
    #[serde(default)]
    pub dormant_days: u32,
    /// 보냈거나 실패한 편지를 며칠 남길지 (7~3650, ADR-0073)
    #[serde(default = "default_mail_retention_days")]
    pub mail_retention_days: u32,
    /// 한 글에 지난 1시간 댓글이 이만큼이면 운영 신호. 0 이면 끔 (ADR-0074)
    #[serde(default = "default_signal_comment_burst")]
    pub signal_comment_burst: u32,
    /// 새 운영 신호를 관리자·소유자에게 메일로 알린다 (ADR-0074)
    #[serde(default = "default_true")]
    pub signal_mail: bool,
    /// 링크 카드(`::link[주소]`)의 제목·설명·그림을 서버가 가져온다 (ADR-0084)
    #[serde(default = "default_true")]
    pub link_cards: bool,
    /// 공개 화면 스킨. `SKINS` 중 하나 (ADR-0081)
    #[serde(default = "default_skin")]
    pub skin: String,
}

/// 내장 스킨 `(id, 이름)`. 스킨은 머리글·메뉴·몸통 배치만 바꾸고 안의 화면은 기본 테마를 같이 쓴다.
pub const SKINS: [(&str, &str); 2] = [("default", "기본"), ("community", "커뮤니티")];

fn default_skin() -> String {
    SKINS[0].0.into()
}

/// 사람 확인 질문 (자체 캡차, ADR-0052). 비회원 댓글·회원 가입 폼에 낸다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CaptchaMode {
    /// 내지 않는다 (함정 칸·시간 확인은 그대로)
    Off,
    /// 비회원 댓글과 회원 가입
    #[default]
    Guest,
}

impl CaptchaMode {
    pub const ALL: [Self; 2] = [Self::Off, Self::Guest];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Guest => "guest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// 회원 가입 방식 (GAP M1, ADR-0051).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SignupMode {
    #[default]
    Off,
    Open,
}

impl SignupMode {
    pub const ALL: [Self; 2] = [Self::Off, Self::Open];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Open => "open",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

fn default_toc_min_headings() -> u32 {
    3
}

fn default_stats_retention_days() -> u32 {
    400
}

fn default_mail_retention_days() -> u32 {
    90
}

fn default_signal_comment_burst() -> u32 {
    20
}

fn default_true() -> bool {
    true
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            name: "Maru".into(),
            description: String::new(),
            lang: "ko".into(),
            tz_offset_min: 540,
            posts_per_page: 10,
            naver_verification: String::new(),
            google_verification: String::new(),
            toc_min_headings: default_toc_min_headings(),
            show_views: true,
            show_search: true,
            signup: SignupMode::Off,
            signup_verify: true,
            captcha: CaptchaMode::Guest,
            stats_retention_days: default_stats_retention_days(),
            banned_words: Vec::new(),
            flood_seconds: 0,
            dormant_days: 0,
            mail_retention_days: default_mail_retention_days(),
            signal_comment_burst: default_signal_comment_burst(),
            signal_mail: true,
            link_cards: true,
            skin: default_skin(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    Active,
    Disabled,
}

impl MemberStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// 사람 회원. 비밀번호 해시는 직렬화하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Member {
    pub id: Id,
    pub email: String,
    pub display_name: String,
    #[serde(skip)]
    pub password_hash: Option<String>,
    pub status: MemberStatus,
    /// 메일 인증을 마친 때. 비었으면 (가입 인증이 켜진 사이트에서) 아직 로그인하지 못한다
    pub email_verified_at: Option<String>,
    /// 탈퇴한 때. 탈퇴하면 이메일·비밀번호를 지우고 닉네임을 바꾼다
    pub withdrawn_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Member {
    pub fn has_password(&self) -> bool {
        self.password_hash.is_some()
    }
}

/// 로그인 세션. 쿠키 원문은 저장하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: Id,
    pub token_hash: String,
    pub member_id: Id,
    pub csrf_token: String,
    pub created_at: String,
    pub expires_at: String,
}

/// 1회용 링크의 쓰임새. 쓰임새가 다른 링크로는 들어갈 수 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketPurpose {
    /// 운영자 로그인 링크 (10분)
    Login,
    /// 가입 메일 인증 (24시간)
    Verify,
    /// 비밀번호 재설정 (1시간)
    Reset,
}

impl TicketPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Verify => "verify",
            Self::Reset => "reset",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Login, Self::Verify, Self::Reset]
            .into_iter()
            .find(|p| p.as_str() == s)
    }
}

/// 1회용 링크 (로그인·메일 인증·비밀번호 재설정).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginTicket {
    pub id: Id,
    pub token_hash: String,
    pub member_id: Id,
    pub purpose: TicketPurpose,
    pub created_at: String,
    pub expires_at: String,
    pub used_at: Option<String>,
}

/// 운영자가 살펴볼 일 하나 (ADR-0074). `message` 에는 사용자가 쓴 글(제목 등)이 들어 있을 수 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Signal {
    /// 같은 신호를 두 번 알리지 않는 열쇠 (예: `comment_burst:<글>:<날짜>`)
    pub key: String,
    /// comment_burst, report_crowd, report_stale …
    pub kind: String,
    pub message: String,
    /// 살펴볼 곳 (사이트 안 경로)
    pub url: Option<String>,
    pub count: i64,
}

/// 회원의 마지막 로그인과 휴면 (ADR-0073).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemberActivity {
    pub last_login_at: Option<String>,
    /// 휴면으로 바뀐 때. 없으면 휴면이 아니다
    pub dormant_at: Option<String>,
}

/// 회원의 TOTP 비밀 (ADR-0072). 비밀이 들어 있어 직렬화하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberTotp {
    pub member_id: Id,
    /// base32 (RFC 4648, 패딩 없음)
    pub secret: String,
    /// 없으면 설정 중 (코드를 아직 확인하지 않음)
    pub enabled_at: Option<String>,
    /// 마지막으로 받은 코드의 30초 순번. 이보다 큰 순번만 받는다.
    pub last_step: i64,
    pub created_at: String,
}

/// 비밀번호를 확인한 뒤 TOTP 코드를 기다리는 로그인.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpChallenge {
    pub id: Id,
    pub token_hash: String,
    pub member_id: Id,
    /// 앞 단계 (`password`, `reset`, `verify`). 감사의 로그인 방법에 붙는다.
    pub method: String,
    pub attempts: i64,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MailStatus {
    Queued,
    Sent,
    Failed,
}

impl MailStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Queued, Self::Sent, Self::Failed]
            .into_iter()
            .find(|p| p.as_str() == s)
    }
}

/// 댓글 상태 (`comments.status`). 공개는 `Published` 하나다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CommentStatus {
    Published,
    /// 승인 대기
    Pending,
    Hidden,
    Spam,
    /// 지운 댓글. 대댓글이 있으면 자리만 남고 본문은 비어 있다
    Deleted,
}

impl CommentStatus {
    pub const ALL: [Self; 5] = [
        Self::Published,
        Self::Pending,
        Self::Hidden,
        Self::Spam,
        Self::Deleted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Pending => "pending",
            Self::Hidden => "hidden",
            Self::Spam => "spam",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// 댓글 쓴 사람 (`comments.actor_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CommentAuthor {
    Human,
    Guest,
    Local,
}

impl CommentAuthor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Guest => "guest",
            Self::Local => "local",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "guest" => Some(Self::Guest),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

/// 댓글 한 개 (`comments`). 본문은 사용자 입력이라 에이전트에게는 데이터로만 넘긴다 (원칙 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Comment {
    pub id: Id,
    pub post_id: Id,
    /// 대댓글이면 뿌리 댓글
    pub parent_id: Option<Id>,
    /// 답한 댓글 (뿌리 또는 같은 뿌리 아래 대댓글)
    pub reply_to_id: Option<Id>,
    pub status: CommentStatus,
    /// 평문. 화면에서 이스케이프한다
    pub body: String,
    pub author_id: Option<Id>,
    pub guest_name: Option<String>,
    /// 비회원 비밀번호 해시. 밖으로 내보내지 않는다
    #[serde(skip)]
    #[schemars(skip)]
    pub guest_pw_hash: Option<String>,
    pub actor_type: CommentAuthor,
    pub created_at: String,
    /// 본문을 마지막으로 고친 때 (상태 변경은 audit_log 에만 남는다 → 화면의 「고침」 표시).
    pub updated_at: String,
}

/// 링크 카드 정보의 상태 (`link_previews.status`, ADR-0084)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    /// 가져올 차례
    Pending,
    Ok,
    /// 몇 번 해도 못 가져옴 — 주소와 도메인만 보인다
    Failed,
}

impl LinkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Pending, Self::Ok, Self::Failed]
            .into_iter()
            .find(|p| p.as_str() == s)
    }
}

/// 링크 카드 한 장의 정보. 주소마다 하나, 여러 글이 함께 쓴다. 제목·설명은 남의 페이지에서 읽은 값이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LinkPreview {
    pub url: String,
    pub status: LinkStatus,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    /// 이 사이트 보관소에 다시 저장한 그림 (`/media/…`)
    pub image_url: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub fetched_at: Option<String>,
    pub created_at: String,
}

/// 보낼 편지함의 편지 한 통 (`mail_outbox`). 본문에 1회용 링크가 들 수 있어 REST 로 내보내지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MailOut {
    pub id: Id,
    /// 같은 값은 한 번만 쌓인다 (예: `verify:<회원>:<10분 칸>`)
    pub idem_key: String,
    pub to_addr: String,
    pub subject: String,
    pub body: String,
    pub status: MailStatus,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub sent_at: Option<String>,
}
