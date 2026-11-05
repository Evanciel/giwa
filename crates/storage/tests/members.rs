//! 회원 가입·메일 인증·비밀번호 찾기·내 정보·탈퇴·관리 (GAP M1·M2·M3·S5, ADR-0051).
//! 실제 SQLite 파일로 같은 답(계정 유무가 새지 않음)·1회용 링크·세션 끊김·익명화를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::Actor;
use maru_core::model::{MailStatus, MemberStatus, PageReq};
use maru_core::services::anonymous_actor;
use maru_core::services::auth::{self, SignedIn};
use maru_core::services::mail;
use maru_core::services::members::{self, ProfilePatch, SignupInput};
use maru_core::services::settings::{self, SettingsPatch};
use maru_core::{Code, Ctx};
use maru_storage::mailer::FileMailer;
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

const PW: &str = "test-password-1";

struct Env {
    _tmp: tempfile::TempDir,
    dir: DataDir,
    db: Db,
    ctx: Ctx,
}

async fn env(signup: &str, verify: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
        extensions: maru_core::store::no_extensions(),
    };
    auth::ensure_owner(&ctx, &Actor::local(), "owner@example.test", "운영자")
        .await
        .unwrap();
    settings::update(
        &ctx,
        &Actor::local(),
        SettingsPatch {
            signup: Some(signup.into()),
            signup_verify: Some(verify),
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Env {
        _tmp: tmp,
        dir,
        db,
        ctx,
    }
}

fn input(email: &str, name: &str) -> SignupInput {
    SignupInput {
        email: email.into(),
        display_name: name.into(),
        password: PW.into(),
        captcha: None,
    }
}

/// 편지함에서 그 주소로 간 마지막 편지의 링크 원문 (`/verify/<원문>`, `/reset/<원문>`).
async fn last_link(ctx: &Ctx, to: &str, path: &str) -> String {
    let mails = mail::list(ctx, &Actor::local(), None, 50).await.unwrap();
    let m = mails
        .iter()
        .find(|m| m.to_addr == to && m.body.contains(path))
        .expect("편지");
    let at = m.body.find(path).expect("링크") + path.len();
    m.body[at..at + 64].to_owned()
}

async fn mail_count(ctx: &Ctx, to: &str) -> usize {
    mail::list(ctx, &Actor::local(), None, 200)
        .await
        .unwrap()
        .iter()
        .filter(|m| m.to_addr == to)
        .count()
}

async fn who(ctx: &Ctx, secret: &str) -> SignedIn {
    auth::signed_in(ctx, secret)
        .await
        .unwrap()
        .expect("로그인 상태")
}

#[tokio::test]
async fn signup_closed_by_default_setting() {
    let e = env("off", true).await;
    let err = members::signup(&e.ctx, input("a@example.test", "달빛"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
}

#[tokio::test]
async fn signup_verify_then_login() {
    let e = env("open", true).await;
    let out = members::signup(&e.ctx, input("Moon@Example.test", "달빛"))
        .await
        .unwrap();
    assert!(out.verify_required);
    assert_eq!(mail_count(&e.ctx, "moon@example.test").await, 1);

    // 인증 전 로그인: 비밀번호가 맞으면 안내, 10분 안엔 편지를 더 쌓지 않는다
    let err = auth::login_password(&e.ctx, "moon@example.test", PW)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    assert_eq!(mail_count(&e.ctx, "moon@example.test").await, 1);
    // 틀린 비밀번호는 그냥 실패 (인증 여부를 알려 주지 않는다)
    let err = auth::login_password(&e.ctx, "moon@example.test", "wrong-password")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);

    let raw = last_link(&e.ctx, "moon@example.test", members::VERIFY_PATH).await;
    let new = members::verify_email(&e.ctx, &raw)
        .await
        .unwrap()
        .session()
        .unwrap();
    assert!(new.member.email_verified_at.is_some());
    assert_eq!(who(&e.ctx, &new.secret).await.member.display_name, "달빛");
    // 링크는 한 번만
    assert_eq!(
        members::verify_email(&e.ctx, &raw)
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    // 로그인 링크 자리에 넣어도 안 된다 (쓰임새가 다르다)
    assert!(auth::peek_link(&e.ctx, &raw).await.is_err());
    let s = auth::login_password(&e.ctx, "moon@example.test", PW)
        .await
        .unwrap()
        .session()
        .unwrap();
    let me = who(&e.ctx, &s.secret).await;
    assert_eq!(me.actor.rank(), maru_core::actor::rank::MEMBER);
}

#[tokio::test]
async fn signup_without_verify_logs_in_at_once() {
    let e = env("open", false).await;
    let out = members::signup(&e.ctx, input("sun@example.test", "햇살"))
        .await
        .unwrap();
    assert!(!out.verify_required);
    assert_eq!(mail_count(&e.ctx, "sun@example.test").await, 0);
    auth::login_password(&e.ctx, "sun@example.test", PW)
        .await
        .unwrap();
}

#[tokio::test]
async fn same_answer_for_existing_email_and_unique_names() {
    let e = env("open", true).await;
    let first = members::signup(&e.ctx, input("moon@example.test", "Moon"))
        .await
        .unwrap();
    // 같은 이메일: 같은 답, 회원은 늘지 않고 「이미 가입」 편지가 간다
    let again = members::signup(&e.ctx, input("MOON@example.test", "다른이름"))
        .await
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(e.ctx.store.count_members().await.unwrap(), 2);
    let mails = mail::list(&e.ctx, &Actor::local(), None, 10).await.unwrap();
    assert!(mails.iter().any(|m| m.subject.contains("이미 가입")));
    // 운영자 메일로 가입해도 같은 답
    let owner = members::signup(&e.ctx, input("owner@example.test", "누군가"))
        .await
        .unwrap();
    assert_eq!(owner, first);
    // 닉네임은 대소문자 무시하고 겹치지 않는다
    let err = members::signup(&e.ctx, input("star@example.test", "moon"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Conflict);
    let err = members::signup(&e.ctx, input("star@example.test", "관리자"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    let err = members::signup(&e.ctx, input("star@example.test", "별빛"))
        .await
        .map(|_| ())
        .and(
            members::signup(
                &e.ctx,
                SignupInput {
                    password: "short".into(),
                    ..input("x@example.test", "짧음")
                },
            )
            .await
            .map(|_| ()),
        )
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
}

#[tokio::test]
async fn reset_password_flow() {
    let e = env("open", false).await;
    members::signup(&e.ctx, input("moon@example.test", "달빛"))
        .await
        .unwrap();
    let old = auth::login_password(&e.ctx, "moon@example.test", PW)
        .await
        .unwrap()
        .session()
        .unwrap();

    // 없는 계정도 같은 답, 편지는 없다
    members::request_reset(&e.ctx, "nobody@example.test")
        .await
        .unwrap();
    members::request_reset(&e.ctx, "not an email")
        .await
        .unwrap();
    assert_eq!(mail_count(&e.ctx, "nobody@example.test").await, 0);

    members::request_reset(&e.ctx, "moon@example.test")
        .await
        .unwrap();
    members::request_reset(&e.ctx, "moon@example.test")
        .await
        .unwrap();
    assert_eq!(
        mail_count(&e.ctx, "moon@example.test").await,
        1,
        "10분 안엔 한 통"
    );
    let raw = last_link(&e.ctx, "moon@example.test", members::RESET_PATH).await;
    assert_eq!(
        members::peek_reset(&e.ctx, &raw)
            .await
            .unwrap()
            .display_name,
        "달빛"
    );
    // 짧은 비밀번호는 링크를 쓰지 않고 거절
    assert_eq!(
        members::reset_password(&e.ctx, &raw, "short")
            .await
            .unwrap_err()
            .code(),
        Code::Validation
    );
    let new = members::reset_password(&e.ctx, &raw, "brand-new-password")
        .await
        .unwrap()
        .session()
        .unwrap();
    assert!(
        auth::signed_in(&e.ctx, &old.secret)
            .await
            .unwrap()
            .is_none(),
        "옛 세션은 끊긴다"
    );
    assert!(
        auth::signed_in(&e.ctx, &new.secret)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        members::reset_password(&e.ctx, &raw, "another-password")
            .await
            .is_err()
    );
    assert!(
        auth::login_password(&e.ctx, "moon@example.test", PW)
            .await
            .is_err()
    );
    auth::login_password(&e.ctx, "moon@example.test", "brand-new-password")
        .await
        .unwrap();
}

#[tokio::test]
async fn profile_password_and_withdraw() {
    let e = env("open", false).await;
    members::signup(&e.ctx, input("moon@example.test", "달빛"))
        .await
        .unwrap();
    members::signup(&e.ctx, input("star@example.test", "별빛"))
        .await
        .unwrap();
    let s = auth::login_password(&e.ctx, "moon@example.test", PW)
        .await
        .unwrap()
        .session()
        .unwrap();
    let me = who(&e.ctx, &s.secret).await;

    let patch = |n: &str| ProfilePatch {
        display_name: Some(n.into()),
        bio: None,
        signature: None,
    };
    assert_eq!(
        members::update_profile(&e.ctx, &me, patch("별빛"))
            .await
            .unwrap_err()
            .code(),
        Code::Conflict
    );
    let m = members::update_profile(&e.ctx, &me, patch("보름달"))
        .await
        .unwrap();
    assert_eq!(m.display_name, "보름달");
    assert_eq!(
        e.ctx
            .store
            .member_by_name("보름달")
            .await
            .unwrap()
            .unwrap()
            .id,
        me.member.id
    );

    // 비밀번호 바꾸기: 지금 비밀번호 확인
    assert_eq!(
        members::change_password(&e.ctx, &me, "wrong", "next-password-2")
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    let s2 = members::change_password(&e.ctx, &me, PW, "next-password-2")
        .await
        .unwrap();
    assert!(auth::signed_in(&e.ctx, &s.secret).await.unwrap().is_none());
    let me = who(&e.ctx, &s2.secret).await;

    // 탈퇴
    assert!(members::withdraw(&e.ctx, &me, "wrong").await.is_err());
    members::withdraw(&e.ctx, &me, "next-password-2")
        .await
        .unwrap();
    let gone = e
        .ctx
        .store
        .member_by_id(&me.member.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gone.status, MemberStatus::Disabled);
    assert!(gone.withdrawn_at.is_some());
    assert!(gone.display_name.starts_with("탈퇴한 회원 #"));
    assert!(gone.email.ends_with("@invalid"));
    assert!(!gone.has_password());
    assert!(e.ctx.store.member_roles(&gone.id).await.unwrap().is_empty());
    assert!(auth::signed_in(&e.ctx, &s2.secret).await.unwrap().is_none());
    assert!(
        auth::login_password(&e.ctx, "moon@example.test", "next-password-2")
            .await
            .is_err()
    );
    // 같은 메일·닉네임으로 다시 가입할 수 있다
    members::signup(&e.ctx, input("moon@example.test", "달빛"))
        .await
        .unwrap();
    assert_eq!(e.ctx.store.count_members().await.unwrap(), 4);

    // 운영자는 탈퇴 못 한다
    let link = auth::issue_login_link(&e.ctx, &Actor::local(), Some("owner@example.test"))
        .await
        .unwrap();
    let raw = link.path.rsplit('/').next().unwrap().to_owned();
    let owner = auth::consume_link(&e.ctx, &raw).await.unwrap();
    let owner = who(&e.ctx, &owner.secret).await;
    assert_eq!(
        members::withdraw(&e.ctx, &owner, "x")
            .await
            .unwrap_err()
            .code(),
        Code::Forbidden
    );
}

#[tokio::test]
async fn admin_status_and_role() {
    let e = env("open", false).await;
    members::signup(&e.ctx, input("moon@example.test", "달빛"))
        .await
        .unwrap();
    let s = auth::login_password(&e.ctx, "moon@example.test", PW)
        .await
        .unwrap()
        .session()
        .unwrap();
    let local = Actor::local();

    let guest = anonymous_actor(&e.ctx).await.unwrap();
    assert_eq!(
        members::list(&e.ctx, &guest, PageReq::new(1, 20))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    let list = members::list(&e.ctx, &local, PageReq::new(1, 20))
        .await
        .unwrap();
    assert_eq!(list.total, 2);
    assert_eq!(list.items[1].roles, ["member"]);

    // 운영자 계정은 바꾸지 못한다
    let err = members::set_status(&e.ctx, &local, "owner@example.test", MemberStatus::Disabled)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    // 비활성 → 세션 끊김, 멱등
    let v = members::set_status(&e.ctx, &local, "moon@example.test", MemberStatus::Disabled)
        .await
        .unwrap();
    assert_eq!(v.member.status, MemberStatus::Disabled);
    assert!(auth::signed_in(&e.ctx, &s.secret).await.unwrap().is_none());
    members::set_status(&e.ctx, &local, "moon@example.test", MemberStatus::Disabled)
        .await
        .unwrap();
    members::set_status(&e.ctx, &local, &v.member.id, MemberStatus::Active)
        .await
        .unwrap();

    let v = members::set_role(&e.ctx, &local, "moon@example.test", "editor")
        .await
        .unwrap();
    assert_eq!(v.roles, ["editor"]);
    let err = members::set_role(&e.ctx, &local, "moon@example.test", "owner")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Validation);
    // 편집자 이하가 관리자를 줄 수는 없다
    let editor_admin = Actor::human("x".into(), [maru_core::actor::Permission::MemberManage], 30);
    let err = members::set_role(&e.ctx, &editor_admin, "moon@example.test", "admin")
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action IN ('member.status', 'member.role')",
    )
    .fetch_one(e.db.read_pool())
    .await
    .unwrap();
    assert_eq!(audits, 3, "바뀐 것만 감사 (멱등 호출은 남기지 않는다)");
}

#[tokio::test]
async fn mail_delivery_to_files_and_no_secrets_in_audit() {
    let e = env("open", true).await;
    members::signup(&e.ctx, input("moon@example.test", "달빛"))
        .await
        .unwrap();
    members::request_reset(&e.ctx, "moon@example.test")
        .await
        .unwrap();
    let verify = last_link(&e.ctx, "moon@example.test", members::VERIFY_PATH).await;

    let mailer = FileMailer::new(e.dir.mail_dir(), &e.ctx.site_url);
    let report = mail::deliver_pending(&e.ctx, &mailer).await.unwrap();
    assert_eq!(report.sent, 2);
    assert_eq!(
        mail::deliver_pending(&e.ctx, &mailer).await.unwrap().sent,
        0,
        "두 번 보내지 않는다"
    );
    let files: Vec<_> = std::fs::read_dir(e.dir.mail_dir()).unwrap().collect();
    assert_eq!(files.len(), 2);
    let all: String = files
        .into_iter()
        .map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
        .collect();
    assert!(all.contains("To: moon@example.test"));
    assert!(all.contains(&verify));
    assert!(all.contains("Subject: =?UTF-8?B?"));
    let sent = mail::list(&e.ctx, &Actor::local(), Some(MailStatus::Sent), 10)
        .await
        .unwrap();
    assert_eq!(sent.len(), 2);

    // 편지함은 로컬 운영자만
    let guest = anonymous_actor(&e.ctx).await.unwrap();
    assert!(mail::list(&e.ctx, &guest, None, 10).await.is_err());

    // 감사에는 비밀번호·링크·해시가 없다
    let diffs: Vec<String> = sqlx::query_scalar("SELECT diff FROM audit_log")
        .fetch_all(e.db.read_pool())
        .await
        .unwrap();
    let joined = diffs.join("\n");
    assert!(!joined.contains(&verify));
    assert!(!joined.contains(PW));
    assert!(!joined.contains("argon2"));
}
