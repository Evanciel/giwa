//! 도메인 모델. 저장소는 이 타입으로 읽고 쓴다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::actor::{AuthorType, Permission, Scope};
use crate::clock::Id;

/// 게시판 종류 (SPEC §4). DB 에 CHECK 를 두지 않고 여기서 검증한다 (ADR-0016).
pub const BOARD_KINDS: [&str; 6] = ["blog", "notice", "gallery", "free", "qna", "faq"];

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

impl Default for BoardPolicy {
    fn default() -> Self {
        Self {
            list: guest(),
            read: guest(),
            write: member(),
            moderate: editor(),
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
    pub policy: BoardPolicy,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
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
}

impl Post {
    /// 공개 여부 판단은 이것 하나다. 게시판 읽기 권한은 별도로 본다.
    pub fn is_public(&self) -> bool {
        self.status == PostStatus::Published && self.kind == PostKind::Post
    }
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

/// 메뉴 위치.
pub const MENU_LOCATIONS: [&str; 2] = ["header", "footer"];

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

/// 1회용 로그인 링크.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginTicket {
    pub id: Id,
    pub token_hash: String,
    pub member_id: Id,
    pub created_at: String,
    pub expires_at: String,
    pub used_at: Option<String>,
}
