//! 요청 주체와 권한 (PHASE1_PLAN §4.3, SPEC §5).
//!
//! 권한 판단은 이 모듈과 서비스 레이어에서만 한다. REST·CLI·웹은 Actor 를 만들어 넘기기만 한다.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::clock::Id;
use crate::error::{Code, Error, Result};

/// 역할 액션 (SPEC §5.1). DB 의 `roles.permissions` 문자열과 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Permission {
    SiteView,
    BoardList,
    PostRead,
    PostWrite,
    PostComment,
    PostUpload,
    PostPublish,
    PostModerate,
    StructureEdit,
    DesignEdit,
    ModuleManage,
    MemberManage,
    SiteAdmin,
}

impl Permission {
    pub const ALL: [Permission; 13] = [
        Self::SiteView,
        Self::BoardList,
        Self::PostRead,
        Self::PostWrite,
        Self::PostComment,
        Self::PostUpload,
        Self::PostPublish,
        Self::PostModerate,
        Self::StructureEdit,
        Self::DesignEdit,
        Self::ModuleManage,
        Self::MemberManage,
        Self::SiteAdmin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SiteView => "site.view",
            Self::BoardList => "board.list",
            Self::PostRead => "post.read",
            Self::PostWrite => "post.write",
            Self::PostComment => "post.comment",
            Self::PostUpload => "post.upload",
            Self::PostPublish => "post.publish",
            Self::PostModerate => "post.moderate",
            Self::StructureEdit => "structure.edit",
            Self::DesignEdit => "design.edit",
            Self::ModuleManage => "module.manage",
            Self::MemberManage => "member.manage",
            Self::SiteAdmin => "site.admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// 에이전트 토큰 스코프 (SPEC §5.3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Draft,
    Publish,
    Structure,
    Design,
    Admin,
}

impl Scope {
    pub const ALL: [Scope; 6] = [
        Self::Read,
        Self::Draft,
        Self::Publish,
        Self::Structure,
        Self::Design,
        Self::Admin,
    ];
    /// 사용자가 따로 고르지 않았을 때 발급하는 스코프.
    pub const DEFAULT: [Scope; 2] = [Self::Read, Self::Draft];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Draft => "draft",
            Self::Publish => "publish",
            Self::Structure => "structure",
            Self::Design => "design",
            Self::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    fn permissions(self) -> &'static [Permission] {
        use Permission::*;
        match self {
            Self::Read => &[SiteView, BoardList, PostRead],
            Self::Draft => &[PostWrite, PostUpload],
            Self::Publish => &[PostPublish, PostModerate],
            Self::Structure => &[StructureEdit],
            Self::Design => &[DesignEdit],
            Self::Admin => &[ModuleManage, MemberManage],
        }
    }
}

/// 게시판 정책 비교용 서열. `roles.rank` 와 같은 값.
pub mod rank {
    pub const GUEST: i64 = 0;
    pub const MEMBER: i64 = 10;
    pub const EDITOR: i64 = 20;
    pub const ADMIN: i64 = 30;
    pub const OWNER: i64 = 40;

    pub fn of_role(name: &str) -> Option<i64> {
        Some(match name {
            "guest" => GUEST,
            "member" => MEMBER,
            "editor" => EDITOR,
            "admin" => ADMIN,
            "owner" => OWNER,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorKind {
    Anonymous,
    Human {
        member_id: Id,
    },
    Agent {
        token_id: Id,
    },
    /// 데이터 디렉터리를 직접 연 로컬 CLI (ADR, Q2). 운영자 권한.
    Local,
}

/// 글의 작성 주체 (`posts.actor_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthorType {
    Human,
    Agent,
    Local,
}

impl AuthorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Local => "local",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "agent" => Some(Self::Agent),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Actor {
    kind: ActorKind,
    perms: BTreeSet<Permission>,
    rank: i64,
    /// 토큰이 게시판을 제한할 때만 `Some`.
    board_ids: Option<Vec<Id>>,
}

impl Actor {
    pub fn anonymous(perms: impl IntoIterator<Item = Permission>) -> Self {
        Self {
            kind: ActorKind::Anonymous,
            perms: perms.into_iter().collect(),
            rank: rank::GUEST,
            board_ids: None,
        }
    }

    pub fn local() -> Self {
        Self {
            kind: ActorKind::Local,
            perms: Permission::ALL.into_iter().collect(),
            rank: rank::OWNER,
            board_ids: None,
        }
    }

    pub fn human(member_id: Id, perms: impl IntoIterator<Item = Permission>, rank: i64) -> Self {
        Self {
            kind: ActorKind::Human { member_id },
            perms: perms.into_iter().collect(),
            rank,
            board_ids: None,
        }
    }

    /// 토큰 스코프로 권한을 정한다. 서열은 게시판 정책 비교에만 쓴다.
    ///
    /// 토큰은 운영자가 발급하므로 쓰기 스코프가 있으면 편집자 서열로 본다.
    /// 그래야 기본 토큰(read, draft)이 블로그(write=editor)에 초안을 쓸 수 있다.
    /// 발행·남의 글 관리는 서열이 아니라 `publish` 스코프의 권한으로 막힌다.
    pub fn agent(token_id: Id, scopes: &[Scope], board_ids: Option<Vec<Id>>) -> Self {
        let perms: BTreeSet<Permission> = scopes
            .iter()
            .flat_map(|s| s.permissions().iter().copied())
            .collect();
        let rank = if scopes.contains(&Scope::Admin) {
            rank::ADMIN
        } else if scopes.contains(&Scope::Publish) || scopes.contains(&Scope::Draft) {
            rank::EDITOR
        } else {
            rank::MEMBER
        };
        Self {
            kind: ActorKind::Agent { token_id },
            perms,
            rank,
            board_ids,
        }
    }

    pub fn kind(&self) -> &ActorKind {
        &self.kind
    }

    pub fn rank(&self) -> i64 {
        self.rank
    }

    pub fn has(&self, perm: Permission) -> bool {
        self.perms.contains(&perm)
    }

    pub fn require(&self, perm: Permission) -> Result<()> {
        if self.has(perm) {
            Ok(())
        } else {
            Err(self.denied(perm))
        }
    }

    /// 비로그인이면 `UNAUTHORIZED`(로그인하면 될 수 있음), 아니면 `FORBIDDEN`.
    pub fn denied(&self, perm: Permission) -> Error {
        match self.kind {
            ActorKind::Anonymous => Error::new(Code::Unauthorized).arg("permission", perm.as_str()),
            _ => Error::new(Code::Forbidden)
                .with_message("error.forbidden_permission")
                .arg("permission", perm.as_str()),
        }
    }

    /// 게시판 서열 조건. `min_role` 이 알 수 없는 이름이면 거부한다 (닫힌 쪽으로 실패).
    pub fn require_rank(&self, perm: Permission, min_role: &str) -> Result<()> {
        match rank::of_role(min_role) {
            Some(min) if self.rank >= min => Ok(()),
            _ => Err(self.denied(perm)),
        }
    }

    pub fn require_board(&self, perm: Permission, board_id: &str) -> Result<()> {
        match &self.board_ids {
            Some(ids) if !ids.iter().any(|id| id == board_id) => Err(Error::new(Code::Forbidden)
                .with_message("error.forbidden_board")
                .arg("permission", perm.as_str())),
            _ => Ok(()),
        }
    }

    pub fn member_id(&self) -> Option<&Id> {
        match &self.kind {
            ActorKind::Human { member_id } => Some(member_id),
            _ => None,
        }
    }

    pub fn author_type(&self) -> Option<AuthorType> {
        match self.kind {
            ActorKind::Anonymous => None,
            ActorKind::Human { .. } => Some(AuthorType::Human),
            ActorKind::Agent { .. } => Some(AuthorType::Agent),
            ActorKind::Local => Some(AuthorType::Local),
        }
    }

    /// `audit_log.actor_type`, `audit_log.actor_id`
    pub fn audit_identity(&self) -> (&'static str, Option<Id>) {
        match &self.kind {
            ActorKind::Anonymous => ("anonymous", None),
            ActorKind::Human { member_id } => ("human", Some(member_id.clone())),
            ActorKind::Agent { token_id } => ("agent", Some(token_id.clone())),
            ActorKind::Local => ("local", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_strings_roundtrip() {
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.as_str()), Some(p));
        }
        for s in Scope::ALL {
            assert_eq!(Scope::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn default_agent_cannot_publish() {
        let a = Actor::agent("t".into(), &Scope::DEFAULT, None);
        assert!(a.has(Permission::PostWrite));
        assert!(!a.has(Permission::PostPublish));
        assert_eq!(a.rank(), rank::EDITOR);
        assert_eq!(
            a.require(Permission::PostPublish).unwrap_err().code(),
            Code::Forbidden
        );
    }

    #[test]
    fn anonymous_denial_is_unauthorized() {
        let a = Actor::anonymous([Permission::PostRead]);
        assert_eq!(
            a.require(Permission::PostWrite).unwrap_err().code(),
            Code::Unauthorized
        );
    }

    #[test]
    fn unknown_role_fails_closed() {
        assert!(
            Actor::local()
                .require_rank(Permission::PostRead, "nobody")
                .is_err()
        );
        assert!(
            Actor::local()
                .require_rank(Permission::PostRead, "owner")
                .is_ok()
        );
    }

    #[test]
    fn board_restriction() {
        let a = Actor::agent("t".into(), &Scope::DEFAULT, Some(vec!["b1".into()]));
        assert!(a.require_board(Permission::PostWrite, "b1").is_ok());
        assert!(a.require_board(Permission::PostWrite, "b2").is_err());
    }
}
