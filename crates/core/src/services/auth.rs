//! 사람 로그인 (ADR-0004, ADR-0009).
//!
//! - 비밀번호는 argon2id. 없는 계정·비밀번호 없는 계정도 더미 해시로 같은 시간을 쓴다.
//! - 세션·로그인 링크 원문은 응답으로 한 번만 내보내고 SHA-256 해시만 저장한다.
//! - 로그인 링크는 10분, 1회. 사용 처리는 한 문장 UPDATE 라 동시에 눌러도 한 번만 성공한다.
//! - 비밀번호 변경·회원 비활성은 그 회원의 모든 세션을 지운다.
//! - 2단계 인증을 켠 회원은 비밀번호·메일 링크 뒤에 코드를 더 받는다(`Login::Totp`, ADR-0072). 운영자 로그인 링크는 거치지 않는다.

use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use serde_json::json;
use time::Duration;

use crate::actor::{Actor, ActorKind, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{LoginTicket, Member, MemberStatus, Session, TicketPurpose};
use crate::services::boards::Created;
use crate::services::{required_text, tokens, totp};
use crate::store::{Ctx, Tx};

pub const SESSION_DAYS: i64 = 14;
pub const TICKET_MINUTES: i64 = 10;
pub const MIN_PASSWORD_CHARS: usize = 10;
pub const MAX_PASSWORD_CHARS: usize = 256;

/// 새로 만든 세션. `secret` 은 쿠키 값이고 다시 볼 수 없다.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub secret: String,
    pub session: Session,
    pub member: Member,
}

/// 새로 만든 로그인 링크. `path` 는 `/admin/link/<원문>`.
#[derive(Debug, Clone)]
pub struct IssuedLink {
    pub path: String,
    pub url: String,
    pub expires_at: String,
    pub member: Member,
}

/// 로그인 결과. 2단계 인증을 켠 회원은 세션 대신 코드 확인 단계를 받는다.
#[derive(Debug, Clone)]
pub enum Login {
    Session(NewSession),
    Totp(totp::Pending),
}

impl Login {
    /// 세션이 바로 만들어졌으면 그것.
    pub fn session(self) -> Option<NewSession> {
        match self {
            Self::Session(s) => Some(s),
            Self::Totp(_) => None,
        }
    }
}

/// 로그인한 사람.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub actor: Actor,
    pub session: Session,
    pub member: Member,
}

/// 32바이트 무작위 값의 hex. 세션·링크·CSRF 원문에 쓴다.
pub fn new_secret() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| Error::internal(e.to_string()))?;
    Ok(hex::encode(buf))
}

fn later(minutes: i64) -> String {
    clock::format(time::OffsetDateTime::now_utc() + Duration::minutes(minutes))
}

/// 첫 운영자를 만든다. 같은 이메일이 있으면 그대로 돌려준다.
/// Phase 1 은 운영자 한 명만 둔다. 다른 회원이 이미 있으면 거부한다.
pub async fn ensure_owner(
    ctx: &Ctx,
    actor: &Actor,
    email: &str,
    display_name: &str,
) -> Result<Created<Member>> {
    require_operator(actor)?;
    let email = check_email(email)?;
    let display_name = required_text("display_name", display_name, 60)?;
    let mut tx = Tx::begin(ctx).await?;
    if let Some(m) = tx.member_by_email(&email).await? {
        return Ok(Created {
            created: false,
            item: m,
        });
    }
    if ctx.store.count_members().await? > 0 {
        return Err(Error::new(Code::Conflict)
            .with_message("error.owner_exists")
            .next("maru login-link"));
    }
    let now = clock::now();
    let member = Member {
        id: clock::new_id(),
        email,
        display_name,
        password_hash: None,
        status: MemberStatus::Active,
        email_verified_at: Some(now.clone()),
        withdrawn_at: None,
        created_at: now.clone(),
        updated_at: now,
    };
    tx.insert_member(&member).await?;
    tx.add_member_role(&member.id, "owner").await?;
    tx.audit(
        actor,
        "member.create",
        Target::new("member", &member.id),
        json!({ "email": member.email, "role": "owner" }),
    )
    .await?;
    tx.commit().await?;
    Ok(Created {
        created: true,
        item: member,
    })
}

/// 로그인 링크를 발급한다. 이전에 발급한 쓰지 않은 링크는 무효가 된다.
/// `email` 이 없으면 회원이 한 명일 때 그 회원에게 발급한다.
pub async fn issue_login_link(ctx: &Ctx, actor: &Actor, email: Option<&str>) -> Result<IssuedLink> {
    require_operator(actor)?;
    let member = match email {
        Some(e) => ctx.store.member_by_email(&check_email(e)?).await?,
        None if ctx.store.count_members().await? == 1 => ctx.store.first_member().await?,
        None => {
            return Err(Error::new(Code::Validation)
                .with_message("error.member_email_required")
                .next("maru login-link --email <이메일>"));
        }
    }
    .ok_or_else(|| {
        Error::new(Code::NotFound)
            .with_message("error.member_not_found")
            .next("maru init --yes")
    })?;
    if member.status != MemberStatus::Active {
        return Err(Error::new(Code::Forbidden).with_message("error.member_disabled"));
    }
    let raw = new_secret()?;
    let mut tx = Tx::begin(ctx).await?;
    tx.delete_open_tickets(&member.id, TicketPurpose::Login)
        .await?;
    let ticket =
        insert_ticket(&mut tx, &member, TicketPurpose::Login, &raw, TICKET_MINUTES).await?;
    tx.audit(
        actor,
        "auth.login_link",
        Target::new("member", &member.id),
        json!({ "expires_at": ticket.expires_at }),
    )
    .await?;
    tx.commit().await?;
    let path = format!("/admin/link/{raw}");
    Ok(IssuedLink {
        url: format!("{}{path}", ctx.site_url.trim_end_matches('/')),
        path,
        expires_at: ticket.expires_at,
        member,
    })
}

/// 확인 화면용. 링크를 쓰지 않고 유효한지만 본다.
pub async fn peek_link(ctx: &Ctx, raw: &str) -> Result<Member> {
    peek_ticket(ctx, raw, TicketPurpose::Login)
        .await?
        .ok_or_else(link_invalid)
}

/// 쓰임새가 맞고, 쓰지 않았고, 만료되지 않은 링크의 활성 회원.
pub(crate) async fn peek_ticket(
    ctx: &Ctx,
    raw: &str,
    purpose: TicketPurpose,
) -> Result<Option<Member>> {
    let now = clock::now();
    let Some(ticket) = ctx
        .store
        .ticket_by_hash(&tokens::hash(raw))
        .await?
        .filter(|t| t.purpose == purpose && t.used_at.is_none() && t.expires_at > now)
    else {
        return Ok(None);
    };
    active_member(ctx, &ticket.member_id).await
}

/// 1회용 링크를 저장한다. 원문은 부른 쪽이 편지·응답으로 한 번만 내보낸다.
pub(crate) async fn insert_ticket(
    tx: &mut Tx,
    member: &Member,
    purpose: TicketPurpose,
    raw: &str,
    minutes: i64,
) -> Result<LoginTicket> {
    let ticket = LoginTicket {
        id: clock::new_id(),
        token_hash: tokens::hash(raw),
        member_id: member.id.clone(),
        purpose,
        created_at: clock::now(),
        expires_at: later(minutes),
        used_at: None,
    };
    tx.insert_ticket(&ticket).await?;
    Ok(ticket)
}

/// 링크를 써서 로그인한다.
pub async fn consume_link(ctx: &Ctx, raw: &str) -> Result<NewSession> {
    let now = clock::now();
    let mut tx = Tx::begin(ctx).await?;
    let ticket = tx
        .use_ticket(&tokens::hash(raw), TicketPurpose::Login, &now)
        .await?
        .ok_or_else(link_invalid)?;
    let member = tx
        .member_by_id(&ticket.member_id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(link_invalid)?;
    let new = start_session(&mut tx, member, "link").await?;
    tx.commit().await?;
    Ok(new)
}

/// 비밀번호로 로그인한다. 실패 사유는 구분하지 않는다.
/// 비밀번호가 맞아도 메일 인증 전이면(가입 인증이 켜졌을 때) 인증 편지를 다시 쌓고 `error.email_unverified`.
pub async fn login_password(ctx: &Ctx, email: &str, password: &str) -> Result<Login> {
    let member = match check_email(email) {
        Ok(e) => ctx.store.member_by_email(&e).await?,
        Err(_) => None,
    };
    let hash = member
        .as_ref()
        .and_then(|m| m.password_hash.clone())
        .unwrap_or_else(|| dummy_hash().to_owned());
    let ok = verify(password, &hash);
    let member = match member {
        Some(m) if ok && m.has_password() && m.status == MemberStatus::Active => m,
        _ => return Err(login_failed()),
    };
    let mut tx = Tx::begin(ctx).await?;
    if member.email_verified_at.is_none() {
        let site = crate::services::settings::site(ctx).await?;
        if site.signup_verify {
            if crate::services::members::queue_verify(ctx, &mut tx, &site.name, &member).await? {
                tx.audit(
                    &Actor::human(member.id.clone(), [], crate::actor::rank::GUEST),
                    "member.verify_resend",
                    Target::new("member", &member.id),
                    json!({}),
                )
                .await?;
                tx.commit().await?;
            }
            return Err(Error::new(Code::Forbidden).with_message("error.email_unverified"));
        }
    }
    let login = finish_login(&mut tx, member, "password").await?;
    tx.commit().await?;
    Ok(login)
}

/// 쿠키 값으로 로그인 상태를 찾는다. 만료·비활성이면 `None`.
pub async fn signed_in(ctx: &Ctx, raw: &str) -> Result<Option<SignedIn>> {
    if raw.is_empty() || raw.len() > 128 {
        return Ok(None);
    }
    let now = clock::now();
    let Some(session) = ctx
        .store
        .session_by_hash(&tokens::hash(raw))
        .await?
        .filter(|s| s.expires_at > now)
    else {
        return Ok(None);
    };
    let Some(member) = active_member(ctx, &session.member_id).await? else {
        return Ok(None);
    };
    Ok(Some(SignedIn {
        actor: member_actor(ctx, &member).await?,
        session,
        member,
    }))
}

/// 회원의 지금 역할로 만든 행위자. 받는 사람이 글을 볼 수 있는지 따질 때도 쓴다.
pub async fn member_actor(ctx: &Ctx, member: &Member) -> Result<Actor> {
    let roles = ctx.store.member_roles(&member.id).await?;
    let rank = roles
        .iter()
        .map(|r| r.rank)
        .max()
        .unwrap_or(crate::actor::rank::GUEST);
    let perms: Vec<Permission> = roles.into_iter().flat_map(|r| r.permissions).collect();
    Ok(Actor::human(member.id.clone(), perms, rank))
}

pub async fn logout(ctx: &Ctx, who: &SignedIn) -> Result<()> {
    let mut tx = Tx::begin(ctx).await?;
    tx.delete_session(&who.session.id).await?;
    tx.audit(
        &who.actor,
        "auth.logout",
        Target::new("member", &who.member.id),
        json!({}),
    )
    .await?;
    tx.commit().await
}

/// 자기 비밀번호를 정한다. 그 회원의 다른 세션은 모두 끊기고 새 세션을 돌려준다.
pub async fn set_password(ctx: &Ctx, who: &SignedIn, password: &str) -> Result<NewSession> {
    let hash = hash_password(password)?;
    let mut tx = Tx::begin(ctx).await?;
    let mut member = tx
        .member_by_id(&who.member.id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(|| Error::new(Code::Unauthorized))?;
    member.password_hash = Some(hash);
    member.updated_at = clock::now();
    tx.update_member(&member).await?;
    tx.delete_member_sessions(&member.id).await?;
    tx.audit(
        &who.actor,
        "auth.password",
        Target::new("member", &member.id),
        json!({ "password": "changed" }),
    )
    .await?;
    let new = start_session(&mut tx, member, "password_change").await?;
    tx.commit().await?;
    Ok(new)
}

/// 만료된 세션·링크 정리. 서버가 주기적으로 부른다.
pub async fn purge_expired(ctx: &Ctx) -> Result<u64> {
    ctx.store.purge_expired(&clock::now()).await
}

/// 상수 시간 비교 (CSRF 값).
pub fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 길이를 검사하고 argon2id 해시를 만든다.
pub(crate) fn hash_password(password: &str) -> Result<String> {
    let n = password.chars().count();
    if !(MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&n) {
        return Err(Error::new(Code::Validation)
            .with_message("error.password_length")
            .arg("min", MIN_PASSWORD_CHARS.to_string())
            .arg("max", MAX_PASSWORD_CHARS.to_string()));
    }
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| Error::internal(e.to_string()))?
        .to_string())
}

/// 길이 검사 없이 argon2id 해시 (비회원 댓글 비밀번호, ADR-0052).
pub(crate) fn hash_raw(password: &str) -> Result<String> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| Error::internal(e.to_string()))?
        .to_string())
}

/// 세션을 만들거나, 2단계 인증을 켠 회원이면 코드 확인 단계를 만든다.
pub(crate) async fn finish_login(tx: &mut Tx, member: Member, method: &str) -> Result<Login> {
    if tx
        .member_totp(&member.id)
        .await?
        .is_some_and(|t| t.enabled_at.is_some())
    {
        return Ok(Login::Totp(totp::challenge(tx, &member, method).await?));
    }
    Ok(Login::Session(start_session(tx, member, method).await?))
}

pub(crate) async fn start_session(tx: &mut Tx, member: Member, method: &str) -> Result<NewSession> {
    let raw = new_secret()?;
    let now = clock::now();
    let session = Session {
        id: clock::new_id(),
        token_hash: tokens::hash(&raw),
        member_id: member.id.clone(),
        csrf_token: new_secret()?,
        created_at: now,
        expires_at: later(SESSION_DAYS * 24 * 60),
    };
    tx.insert_session(&session).await?;
    let actor = Actor::human(member.id.clone(), [], crate::actor::rank::GUEST);
    tx.audit(
        &actor,
        "auth.login",
        Target::new("member", &member.id),
        json!({ "method": method }),
    )
    .await?;
    Ok(NewSession {
        secret: raw,
        session,
        member,
    })
}

async fn active_member(ctx: &Ctx, id: &str) -> Result<Option<Member>> {
    Ok(ctx
        .store
        .member_by_id(id)
        .await?
        .filter(|m| m.status == MemberStatus::Active))
}

/// 로그인 링크 발급은 운영자(로컬 CLI, 사람 owner)만. 에이전트 토큰은 admin 스코프여도 안 된다.
pub(crate) fn require_operator(actor: &Actor) -> Result<()> {
    actor.require(Permission::SiteAdmin)?;
    if matches!(actor.kind(), ActorKind::Agent { .. }) {
        return Err(actor.denied(Permission::SiteAdmin));
    }
    Ok(())
}

pub(crate) fn check_email(email: &str) -> Result<String> {
    let e = email.trim().to_lowercase();
    let ok = e.len() <= 254
        && e.split_once('@')
            .is_some_and(|(l, d)| !l.is_empty() && !d.is_empty() && !d.contains('@'))
        && !e.chars().any(|c| c.is_whitespace() || c.is_control());
    if !ok {
        return Err(Error::new(Code::Validation)
            .with_message("error.email")
            .arg("value", email.chars().take(60).collect::<String>()));
    }
    Ok(e)
}

pub(crate) fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).ok().is_some_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// 없는 계정에도 같은 계산을 하기 위한 해시.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        Argon2::default()
            .hash_password(b"maru-dummy-password")
            .map(|h| h.to_string())
            .unwrap_or_default()
    })
}

fn link_invalid() -> Error {
    Error::new(Code::Unauthorized)
        .with_message("error.link_invalid")
        .next("maru login-link")
}

fn login_failed() -> Error {
    Error::new(Code::Unauthorized).with_message("error.login_failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip_and_dummy() {
        let h = Argon2::default()
            .hash_password(b"correct horse")
            .unwrap()
            .to_string();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("correct horse", &h));
        assert!(!verify("wrong", &h));
        assert!(!verify("x", dummy_hash()));
        assert!(!verify("x", "not a hash"));
    }

    #[test]
    fn constant_time_compare() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "ab"));
    }

    #[test]
    fn emails_are_normalized() {
        assert_eq!(check_email(" Me@Example.TEST ").unwrap(), "me@example.test");
        assert!(check_email("no-at").is_err());
        assert!(check_email("a@b@c").is_err());
    }
}
