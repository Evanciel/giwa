//! 회원 가입·메일 인증·비밀번호 찾기·내 정보·탈퇴, 관리용 회원 목록 (GAP M1·M2·M3, ADR-0051).
//!
//! - 가입·비밀번호 찾기는 계정이 있든 없든 같은 답을 준다. 이미 있는 이메일에는 「이미 가입돼 있다」 편지를 보낸다.
//! - 인증·재설정 링크는 1회용이고 해시만 저장한다(ADR-0009). 재설정·비밀번호 변경·비활성·탈퇴는 그 회원의 모든 세션을 끊는다.
//! - 같은 사람에게 같은 종류의 편지는 10분에 한 통만 쌓인다(`mail::window`).
//! - 감사에는 비밀번호·링크를 남기지 않는다.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actor::{Actor, Permission, rank};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{
    CaptchaMode, Listing, Member, MemberStatus, PageReq, SignupMode, TicketPurpose,
};
use crate::services::auth::{self, Login, NewSession, SignedIn};
use crate::services::{mail, settings, tokens, totp};
use crate::store::{Ctx, Tx};

pub const VERIFY_HOURS: i64 = 24;
pub const RESET_MINUTES: i64 = 60;
pub const MIN_NAME_CHARS: usize = 2;
pub const MAX_NAME_CHARS: usize = 20;
/// 관리 화면에서 줄 수 있는 역할. 운영자(owner)는 넘기지 않는다.
pub const ASSIGNABLE_ROLES: [&str; 3] = ["member", "editor", "admin"];
const RESERVED_NAMES: [&str; 8] = [
    "운영자",
    "관리자",
    "admin",
    "administrator",
    "root",
    "maru",
    "system",
    "탈퇴한 회원",
];

pub const VERIFY_PATH: &str = "/verify/";
pub const RESET_PATH: &str = "/reset/";

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct SignupInput {
    pub email: String,
    /// 닉네임 (사이트 안에서 겹치지 않게, 2~20자)
    pub display_name: String,
    pub password: String,
    /// 사이트가 확인 질문을 낼 때 (`captcha = guest`, ADR-0052)
    #[serde(default)]
    pub captcha: Option<crate::services::captcha::Answer>,
}

/// 가입 결과. 이미 있는 이메일이어도 같은 모양이다(계정이 있는지 새지 않게).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SignupOutcome {
    /// 메일 링크를 눌러야 로그인할 수 있다
    pub verify_required: bool,
}

/// 관리용 회원 한 명.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MemberView {
    #[serde(flatten)]
    pub member: Member,
    pub roles: Vec<String>,
    /// 2단계 인증을 켰는지 (ADR-0072)
    pub two_factor: bool,
    /// 휴면이면 메일 주소는 따로 보관돼 `email` 이 자리표시 값이다 (ADR-0073)
    pub dormant: bool,
    pub last_login_at: Option<String>,
}

/// 가입한다. 비밀번호 해시 계산이 무거우므로 웹은 동시에 부르는 수를 제한한다.
pub async fn signup(ctx: &Ctx, input: SignupInput) -> Result<SignupOutcome> {
    let site = settings::site(ctx).await?;
    if site.signup != SignupMode::Open {
        return Err(signup_closed());
    }
    let email = auth::check_email(&input.email)?;
    let name = check_name(&input.display_name)?;
    if site.captcha == CaptchaMode::Guest {
        let answer = input.captcha.clone().unwrap_or_default();
        crate::services::captcha::check(ctx, &answer).await?;
    }
    let hash = auth::hash_password(&input.password)?;
    let outcome = SignupOutcome {
        verify_required: site.signup_verify,
    };
    let mut tx = Tx::begin(ctx).await?;
    if let Some(existing) = tx.member_by_email(&email).await? {
        // 이미 있는 이메일: 가입시키지 않고 그 주소로 알림만 보낸다. 답은 똑같다.
        if existing.status == MemberStatus::Active {
            let link = reset_ticket(ctx, &mut tx, &existing).await?;
            let key = format!("exists:{}:{}", existing.id, mail::window());
            if mail::queue(
                &mut tx,
                key,
                &existing.email,
                "exists",
                &mail_args(&site.name, &existing, &link),
            )
            .await?
            {
                tx.audit(
                    &guest_of(&existing),
                    "member.signup_repeat",
                    Target::new("member", &existing.id),
                    json!({}),
                )
                .await?;
                tx.commit().await?;
            }
        }
        return Ok(outcome);
    }
    if tx.member_by_name(&name).await?.is_some() {
        return Err(name_taken(&name));
    }
    let now = clock::now();
    let member = Member {
        id: clock::new_id(),
        email,
        display_name: name,
        password_hash: Some(hash),
        status: MemberStatus::Active,
        email_verified_at: (!site.signup_verify).then(|| now.clone()),
        withdrawn_at: None,
        created_at: now.clone(),
        updated_at: now,
    };
    tx.insert_member(&member).await?;
    tx.add_member_role(&member.id, "member").await?;
    tx.audit(
        &guest_of(&member),
        "member.signup",
        Target::new("member", &member.id),
        json!({ "display_name": member.display_name, "verify": site.signup_verify }),
    )
    .await?;
    if site.signup_verify {
        queue_verify(ctx, &mut tx, &site.name, &member).await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

/// 인증 편지를 쌓는다 (가입 직후, 인증 전 로그인 시도). 10분 안에 다시 부르면 쌓지 않는다.
pub(crate) async fn queue_verify(
    ctx: &Ctx,
    tx: &mut Tx,
    site_name: &str,
    member: &Member,
) -> Result<bool> {
    let key = format!("verify:{}:{}", member.id, mail::window());
    let raw = auth::new_secret()?;
    let link = format!("{}{VERIFY_PATH}{raw}", ctx.site_url);
    let queued = mail::queue(
        tx,
        key,
        &member.email,
        "verify",
        &mail_args(site_name, member, &link),
    )
    .await?;
    if queued {
        tx.delete_open_tickets(&member.id, TicketPurpose::Verify)
            .await?;
        auth::insert_ticket(tx, member, TicketPurpose::Verify, &raw, VERIFY_HOURS * 60).await?;
    }
    Ok(queued)
}

/// 메일 인증 링크를 쓴다. 인증하면 바로 로그인 세션을 만든다(링크가 로그인 링크와 같은 힘을 갖는다).
/// 2단계 인증을 켠 회원이면 세션 대신 코드 확인 단계.
pub async fn verify_email(ctx: &Ctx, raw: &str) -> Result<Login> {
    let mut tx = Tx::begin(ctx).await?;
    let ticket = tx
        .use_ticket(&tokens::hash(raw), TicketPurpose::Verify, &clock::now())
        .await?
        .ok_or_else(ticket_invalid)?;
    let mut member = tx
        .member_by_id(&ticket.member_id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(ticket_invalid)?;
    if member.email_verified_at.is_none() {
        member.email_verified_at = Some(clock::now());
        member.updated_at = clock::now();
        tx.update_member(&member).await?;
    }
    tx.audit(
        &guest_of(&member),
        "member.verify",
        Target::new("member", &member.id),
        json!({}),
    )
    .await?;
    let login = auth::finish_login(&mut tx, member, "verify").await?;
    tx.commit().await?;
    Ok(login)
}

/// 인증 링크가 쓸 수 있는지 (확인 단추를 보이기 전). 메일 미리보기가 GET 으로 링크를 열어도 쓰이지 않게 한다.
pub async fn peek_verify(ctx: &Ctx, raw: &str) -> Result<Member> {
    auth::peek_ticket(ctx, raw, TicketPurpose::Verify)
        .await?
        .ok_or_else(ticket_invalid)
}

/// 비밀번호 재설정 편지를 보낸다. 계정이 없어도, 10분 안에 다시 불러도 같은 답(`Ok`).
pub async fn request_reset(ctx: &Ctx, email: &str) -> Result<()> {
    let Ok(email) = auth::check_email(email) else {
        return Ok(());
    };
    let Some(member) = ctx
        .store
        .member_by_email(&email)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
    else {
        return Ok(());
    };
    let site = settings::site(ctx).await?;
    let mut tx = Tx::begin(ctx).await?;
    let link = reset_ticket(ctx, &mut tx, &member).await?;
    let key = format!("reset:{}:{}", member.id, mail::window());
    if !mail::queue(
        &mut tx,
        key,
        &member.email,
        "reset",
        &mail_args(&site.name, &member, &link),
    )
    .await?
    {
        return Ok(()); // 드롭하면 새 링크도 함께 롤백된다
    }
    tx.audit(
        &guest_of(&member),
        "member.reset_request",
        Target::new("member", &member.id),
        json!({}),
    )
    .await?;
    tx.commit().await
}

/// 재설정 링크가 쓸 수 있는지 (재설정 폼을 보이기 전).
pub async fn peek_reset(ctx: &Ctx, raw: &str) -> Result<Member> {
    auth::peek_ticket(ctx, raw, TicketPurpose::Reset)
        .await?
        .ok_or_else(ticket_invalid)
}

/// 재설정 링크로 새 비밀번호를 정한다. 다른 세션은 모두 끊고 새 세션을 돌려준다.
/// 2단계 인증을 켠 회원이면 비밀번호는 바뀌고 세션 대신 코드 확인 단계 (메일만으로는 들어오지 못한다).
pub async fn reset_password(ctx: &Ctx, raw: &str, password: &str) -> Result<Login> {
    let hash = auth::hash_password(password)?;
    let mut tx = Tx::begin(ctx).await?;
    let ticket = tx
        .use_ticket(&tokens::hash(raw), TicketPurpose::Reset, &clock::now())
        .await?
        .ok_or_else(ticket_invalid)?;
    let mut member = tx
        .member_by_id(&ticket.member_id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(ticket_invalid)?;
    let now = clock::now();
    member.password_hash = Some(hash);
    // 메일로 온 링크를 눌렀으니 주소도 확인된 셈이다.
    member.email_verified_at.get_or_insert_with(|| now.clone());
    member.updated_at = now;
    tx.update_member(&member).await?;
    tx.delete_member_sessions(&member.id).await?;
    tx.audit(
        &guest_of(&member),
        "member.reset",
        Target::new("member", &member.id),
        json!({ "password": "changed" }),
    )
    .await?;
    let login = auth::finish_login(&mut tx, member, "reset").await?;
    tx.commit().await?;
    Ok(login)
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfilePatch {
    #[serde(default)]
    pub display_name: Option<String>,
}

/// 내 정보 고치기 (지금은 닉네임).
pub async fn update_profile(ctx: &Ctx, who: &SignedIn, patch: ProfilePatch) -> Result<Member> {
    let mut tx = Tx::begin(ctx).await?;
    let mut member = active(&mut tx, &who.member.id).await?;
    let before = member.display_name.clone();
    if let Some(n) = &patch.display_name {
        let name = check_name(n)?;
        if let Some(other) = tx.member_by_name(&name).await?
            && other.id != member.id
        {
            return Err(name_taken(&name));
        }
        member.display_name = name;
    }
    if member.display_name == before {
        return Ok(member);
    }
    member.updated_at = clock::now();
    tx.update_member(&member).await?;
    tx.audit(
        &who.actor,
        "member.update",
        Target::new("member", &member.id),
        json!({ "display_name": { "before": before, "after": member.display_name } }),
    )
    .await?;
    tx.commit().await?;
    Ok(member)
}

/// 비밀번호 바꾸기. 지금 비밀번호가 있으면 확인한다. 다른 세션은 모두 끊긴다.
pub async fn change_password(
    ctx: &Ctx,
    who: &SignedIn,
    current: &str,
    new: &str,
) -> Result<NewSession> {
    if let Some(hash) = &who.member.password_hash
        && !auth::verify(current, hash)
    {
        return Err(password_wrong());
    }
    auth::set_password(ctx, who, new).await
}

/// 탈퇴. 이메일·비밀번호·세션·역할을 지우고 닉네임을 바꾼다. 쓴 글은 남는다. 운영자는 탈퇴할 수 없다.
pub async fn withdraw(ctx: &Ctx, who: &SignedIn, password: &str) -> Result<()> {
    if who.actor.has(Permission::SiteAdmin) {
        return Err(Error::new(Code::Forbidden).with_message("error.owner_withdraw"));
    }
    let hash = who.member.password_hash.clone().unwrap_or_default();
    if !auth::verify(password, &hash) {
        return Err(password_wrong());
    }
    let mut tx = Tx::begin(ctx).await?;
    let mut member = active(&mut tx, &who.member.id).await?;
    let now = clock::now();
    let tail: String = member
        .id
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    member.email = format!("withdrawn+{}@invalid", member.id.to_lowercase());
    member.display_name = format!("탈퇴한 회원 #{tail}");
    member.password_hash = None;
    member.status = MemberStatus::Disabled;
    member.withdrawn_at = Some(now.clone());
    member.updated_at = now;
    tx.update_member(&member).await?;
    tx.clear_member_roles(&member.id).await?;
    tx.delete_member_sessions(&member.id).await?;
    tx.delete_member_totp(&member.id).await?;
    for p in [
        TicketPurpose::Login,
        TicketPurpose::Verify,
        TicketPurpose::Reset,
    ] {
        tx.delete_open_tickets(&member.id, p).await?;
    }
    tx.audit(
        &who.actor,
        "member.withdraw",
        Target::new("member", &member.id),
        json!({}),
    )
    .await?;
    tx.commit().await
}

// ---- 관리 (member.manage) ----

pub async fn list(ctx: &Ctx, actor: &Actor, page: PageReq) -> Result<Listing<MemberView>> {
    actor.require(Permission::MemberManage)?;
    let members = ctx.store.list_members(page.limit(), page.offset()).await?;
    let mut items = Vec::with_capacity(members.len());
    for m in members {
        items.push(view(ctx, m).await?);
    }
    Ok(Listing {
        items,
        total: ctx.store.count_members().await?,
        page: page.page,
        per_page: page.per_page,
    })
}

/// 회원 하나. `who` 는 회원 ID 또는 이메일.
pub async fn get(ctx: &Ctx, actor: &Actor, who: &str) -> Result<MemberView> {
    actor.require(Permission::MemberManage)?;
    view(ctx, find(ctx, who).await?).await
}

/// 활성·비활성. 비활성은 그 회원의 세션을 모두 끊는다. 운영자·자기 자신·탈퇴한 회원은 바꾸지 못한다.
pub async fn set_status(
    ctx: &Ctx,
    actor: &Actor,
    who: &str,
    status: MemberStatus,
) -> Result<MemberView> {
    actor.require(Permission::MemberManage)?;
    let target = find(ctx, who).await?;
    guard_target(ctx, actor, &target).await?;
    if target.withdrawn_at.is_some() {
        return Err(Error::new(Code::Conflict).with_message("error.member_withdrawn"));
    }
    if target.status == status {
        return view(ctx, target).await;
    }
    let mut tx = Tx::begin(ctx).await?;
    let mut m = target.clone();
    m.status = status;
    m.updated_at = clock::now();
    tx.update_member(&m).await?;
    if status == MemberStatus::Disabled {
        tx.delete_member_sessions(&m.id).await?;
    }
    tx.audit(
        actor,
        "member.status",
        Target::new("member", &m.id),
        json!({ "before": target.status, "after": status }),
    )
    .await?;
    tx.commit().await?;
    view(ctx, m).await
}

/// 역할을 하나로 정한다 (`member`·`editor`·`admin`). `admin` 은 사이트 관리 권한이 있어야 준다.
pub async fn set_role(ctx: &Ctx, actor: &Actor, who: &str, role: &str) -> Result<MemberView> {
    actor.require(Permission::MemberManage)?;
    if !ASSIGNABLE_ROLES.contains(&role) {
        return Err(Error::new(Code::Validation)
            .with_message("error.member_role")
            .arg("value", role.chars().take(30).collect::<String>())
            .arg("allowed", ASSIGNABLE_ROLES.join(", ")));
    }
    if role == "admin" {
        actor.require(Permission::SiteAdmin)?;
    }
    let target = find(ctx, who).await?;
    guard_target(ctx, actor, &target).await?;
    if target.withdrawn_at.is_some() {
        return Err(Error::new(Code::Conflict).with_message("error.member_withdrawn"));
    }
    let before: Vec<String> = ctx
        .store
        .member_roles(&target.id)
        .await?
        .into_iter()
        .map(|r| r.name)
        .collect();
    if before == [role] {
        return view(ctx, target).await;
    }
    let mut tx = Tx::begin(ctx).await?;
    tx.clear_member_roles(&target.id).await?;
    tx.add_member_role(&target.id, role).await?;
    // 권한이 바뀌었으니 다시 로그인하게 한다.
    tx.delete_member_sessions(&target.id).await?;
    tx.audit(
        actor,
        "member.role",
        Target::new("member", &target.id),
        json!({ "before": before, "after": [role] }),
    )
    .await?;
    tx.commit().await?;
    view(ctx, target).await
}

// ---- 도우미 ----

/// 닉네임: 앞뒤 공백을 지우고 NFC, 2~20자, 제어 문자·`@`·`<>` 없이, 예약어 아님.
pub fn check_name(name: &str) -> Result<String> {
    use unicode_normalization::UnicodeNormalization;
    let n: String = name.trim().nfc().collect();
    let len = n.chars().count();
    let bad_char = n
        .chars()
        .any(|c| c.is_control() || matches!(c, '@' | '<' | '>' | '"' | '\\'));
    if !(MIN_NAME_CHARS..=MAX_NAME_CHARS).contains(&len) || bad_char {
        return Err(Error::new(Code::Validation)
            .with_message("error.name_invalid")
            .arg("min", MIN_NAME_CHARS.to_string())
            .arg("max", MAX_NAME_CHARS.to_string())
            .arg("value", n.chars().take(30).collect::<String>()));
    }
    let lower = n.to_lowercase();
    if RESERVED_NAMES
        .iter()
        .any(|r| lower.starts_with(&r.to_lowercase()))
    {
        return Err(Error::new(Code::Validation)
            .with_message("error.name_reserved")
            .arg("value", n));
    }
    Ok(n)
}

async fn view(ctx: &Ctx, member: Member) -> Result<MemberView> {
    let roles = ctx
        .store
        .member_roles(&member.id)
        .await?
        .into_iter()
        .map(|r| r.name)
        .collect();
    let two_factor = totp::is_enabled(ctx, &member.id).await?;
    let activity = ctx.store.member_activity(&member.id).await?;
    Ok(MemberView {
        member,
        roles,
        two_factor,
        dormant: activity.dormant_at.is_some(),
        last_login_at: activity.last_login_at,
    })
}

pub(crate) async fn find(ctx: &Ctx, who: &str) -> Result<Member> {
    let found = if who.contains('@') {
        match auth::check_email(who) {
            Ok(e) => ctx.store.member_by_email(&e).await?,
            Err(_) => None,
        }
    } else {
        ctx.store.member_by_id(who).await?
    };
    found.ok_or_else(|| {
        Error::new(Code::NotFound)
            .with_message("error.member_not_found")
            .next("maru member list")
    })
}

/// 운영자(owner)·자기 자신은 관리 화면에서 바꾸지 않는다.
async fn guard_target(ctx: &Ctx, actor: &Actor, target: &Member) -> Result<()> {
    if let crate::actor::ActorKind::Human { member_id } = actor.kind()
        && *member_id == target.id
    {
        return Err(Error::new(Code::Forbidden).with_message("error.member_self"));
    }
    let is_owner = ctx
        .store
        .member_roles(&target.id)
        .await?
        .iter()
        .any(|r| r.name == "owner");
    if is_owner {
        return Err(Error::new(Code::Forbidden).with_message("error.member_owner"));
    }
    Ok(())
}

async fn active(tx: &mut Tx, id: &str) -> Result<Member> {
    tx.member_by_id(id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(|| Error::new(Code::Unauthorized))
}

async fn reset_ticket(ctx: &Ctx, tx: &mut Tx, member: &Member) -> Result<String> {
    let raw = auth::new_secret()?;
    tx.delete_open_tickets(&member.id, TicketPurpose::Reset)
        .await?;
    auth::insert_ticket(tx, member, TicketPurpose::Reset, &raw, RESET_MINUTES).await?;
    Ok(format!("{}{RESET_PATH}{raw}", ctx.site_url))
}

fn mail_args(site: &str, member: &Member, link: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("site", site.to_owned()),
        ("name", member.display_name.clone()),
        ("link", link.to_owned()),
    ])
}

/// 로그인 전 사람의 감사 주체 (권한 없음).
fn guest_of(member: &Member) -> Actor {
    Actor::human(member.id.clone(), [], rank::GUEST)
}

fn signup_closed() -> Error {
    Error::new(Code::NotFound).with_message("error.signup_closed")
}

fn name_taken(name: &str) -> Error {
    Error::new(Code::Conflict)
        .with_message("error.name_taken")
        .arg("value", name)
}

fn ticket_invalid() -> Error {
    Error::new(Code::Unauthorized).with_message("error.ticket_invalid")
}

fn password_wrong() -> Error {
    Error::new(Code::Unauthorized).with_message("error.password_wrong")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked() {
        assert_eq!(check_name("  달빛  ").unwrap(), "달빛");
        assert!(check_name("가").is_err());
        assert!(check_name(&"가".repeat(21)).is_err());
        assert!(check_name("a@b").is_err());
        assert!(check_name("관리자님").is_err());
        assert!(check_name("Admin2").is_err());
        assert!(check_name("탈퇴한 회원 #12").is_err());
        // NFD 로 들어온 한글도 같은 닉네임
        assert_eq!(check_name("\u{1100}\u{1161}\u{11a8}가").unwrap(), "각가");
    }
}
