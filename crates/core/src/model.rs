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
