//! 개인정보 보유기간 (GAP M17, ADR-0073). 실제 SQLite 파일로 휴면 미리 알림 → 휴면(메일 주소 분리·
//! 세션 끊김) → 로그인으로 깨우기, 운영진·탈퇴 회원 제외, 지난 편지 지우기, 설정 검사를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use giwa_core::actor::{Actor, Scope};
use giwa_core::model::MailStatus;
use giwa_core::services::auth::{self, Login};
use giwa_core::services::members::{self, SignupInput};
use giwa_core::services::settings::{self, SettingsPatch};
use giwa_core::services::{mail, retention};
use giwa_core::{Code, Ctx};
use giwa_storage::{DataDir, Db, FsBlobs, SqliteStore};

const PW: &str = "test-password-1";
const EMAIL: &str = "moon@example.test";

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
    ctx: Ctx,
    moon: String,
}

async fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let dir = DataDir::new(tmp.path().join("데이터"));
    dir.ensure_layout().unwrap();
    let db = Db::open(&dir.db_path(), true).await.unwrap();
    db.migrate().await.unwrap();
    let ctx = Ctx {
        store: Arc::new(SqliteStore::new(db.clone())),
        blobs: Arc::new(FsBlobs::new(dir.media_dir())),
        site_url: "http://127.0.0.1:8080".into(),
        extensions: giwa_core::store::no_extensions(),
    };
    auth::ensure_owner(&ctx, &Actor::local(), "owner@example.test", "운영자")
        .await
        .unwrap();
    set(
        &ctx,
        SettingsPatch {
            name: Some("달빛 마을".into()),
            signup: Some("open".into()),
            signup_verify: Some(false),
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await;
    members::signup(
        &ctx,
        SignupInput {
            email: EMAIL.into(),
            display_name: "달빛".into(),
            password: PW.into(),
            captcha: None,
        },
    )
    .await
    .unwrap();
    let moon = ctx.store.member_by_email(EMAIL).await.unwrap().unwrap().id;
    Env {
        _tmp: tmp,
        db,
        ctx,
        moon,
    }
}

async fn set(ctx: &Ctx, patch: SettingsPatch) {
    settings::update(ctx, &Actor::local(), patch).await.unwrap();
}

async fn sql(e: &Env, q: &'static str) {
    sqlx::query(q).execute(e.db.write_pool()).await.unwrap();
}

async fn count(e: &Env, q: &'static str) -> i64 {
    sqlx::query_scalar(q)
        .fetch_one(e.db.read_pool())
        .await
        .unwrap()
}

async fn run(e: &Env) -> retention::Report {
    retention::run(&e.ctx, &Actor::local()).await.unwrap()
}

/// 모든 회원의 마지막 접속을 400일 전으로 돌린다.
async fn age_everyone(e: &Env) {
    sql(
        e,
        "UPDATE members SET last_login_at = '2000-01-01T00:00:00Z', created_at = '2000-01-01T00:00:00Z'",
    )
    .await;
}

#[tokio::test]
async fn off_by_default_and_settings_are_checked() {
    let e = env().await;
    age_everyone(&e).await;
    let r = run(&e).await;
    assert_eq!((r.notified, r.dormant), (0, 0), "기본은 끔");
    let s = settings::site(&e.ctx).await.unwrap();
    assert_eq!((s.dormant_days, s.mail_retention_days), (0, 90));

    for bad in [1, 179, 3651] {
        let err = settings::update(
            &e.ctx,
            &Actor::local(),
            SettingsPatch {
                dormant_days: Some(bad),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), Code::Validation, "dormant_days {bad}");
    }
    let err = settings::update(
        &e.ctx,
        &Actor::local(),
        SettingsPatch {
            mail_retention_days: Some(6),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Validation);

    // 회원 관리 권한이 없는 에이전트는 바꾸지도, 돌리지도 못한다
    let agent = Actor::agent("t1".into(), &[Scope::Structure], None);
    let err = settings::update(
        &e.ctx,
        &agent,
        SettingsPatch {
            dormant_days: Some(365),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), Code::Forbidden);
    assert_eq!(
        retention::run(&e.ctx, &agent).await.unwrap_err().code(),
        Code::Forbidden
    );
}

#[tokio::test]
async fn notice_then_dormant_then_wake_on_login() {
    let e = env().await;
    let Login::Session(before) = auth::login_password(&e.ctx, EMAIL, PW).await.unwrap() else {
        panic!("세션");
    };
    let view = members::get(&e.ctx, &Actor::local(), &e.moon)
        .await
        .unwrap();
    assert!(
        view.last_login_at.is_some(),
        "로그인하면 마지막 로그인을 적는다"
    );
    set(
        &e.ctx,
        SettingsPatch {
            dormant_days: Some(365),
            ..Default::default()
        },
    )
    .await;

    // 최근에 로그인했으면 아무 일도 없다
    let r = run(&e).await;
    assert_eq!((r.notified, r.dormant), (0, 0));

    age_everyone(&e).await;
    let r = run(&e).await;
    assert_eq!(r.notified, 1, "운영자는 빠지고 달빛만 알린다");
    assert_eq!(r.dormant, 0, "알린 뒤 30일은 기다린다");
    let mails = mail::list(&e.ctx, &Actor::local(), Some(MailStatus::Queued), 10)
        .await
        .unwrap();
    let notice = mails
        .iter()
        .find(|m| m.subject.contains("휴면"))
        .expect("미리 알림 편지");
    assert_eq!(notice.to_addr, EMAIL);
    assert!(notice.body.contains("365일") && notice.body.contains("/login"));
    assert_eq!(run(&e).await.notified, 0, "한 번만 알린다");

    sql(
        &e,
        "UPDATE members SET dormant_notice_at = '2000-02-01T00:00:00Z' WHERE dormant_notice_at IS NOT NULL",
    )
    .await;
    let r = run(&e).await;
    assert_eq!(r.dormant, 1);
    assert_eq!(run(&e).await.dormant, 0, "여러 번 돌려도 같다");

    let view = members::get(&e.ctx, &Actor::local(), &e.moon)
        .await
        .unwrap();
    assert!(view.dormant);
    assert!(view.member.email.ends_with("@invalid"), "주소는 분리 표로");
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM member_dormant WHERE email = 'moon@example.test'"
        )
        .await,
        1
    );
    assert!(
        auth::signed_in(&e.ctx, &before.secret)
            .await
            .unwrap()
            .is_none(),
        "휴면이면 로그인이 풀린다"
    );
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM audit_log WHERE action = 'member.dormant'"
        )
        .await,
        1
    );

    // 휴면이어도 같은 주소로는 가입하지 못한다(이미 있는 주소로 다룬다)
    members::signup(
        &e.ctx,
        SignupInput {
            email: EMAIL.into(),
            display_name: "다른 사람".into(),
            password: PW.into(),
            captcha: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM members WHERE display_name = '다른 사람'"
        )
        .await,
        0
    );

    // 재설정 편지는 진짜 주소로 간다
    members::request_reset(&e.ctx, EMAIL).await.unwrap();
    let mails = mail::list(&e.ctx, &Actor::local(), Some(MailStatus::Queued), 20)
        .await
        .unwrap();
    assert!(
        mails
            .iter()
            .any(|m| m.subject.contains("비밀번호") && m.to_addr == EMAIL)
    );
    assert!(mails.iter().all(|m| !m.to_addr.ends_with("@invalid")));

    // 비밀번호로 로그인하면 바로 돌아온다
    let Login::Session(back) = auth::login_password(&e.ctx, EMAIL, PW).await.unwrap() else {
        panic!("세션");
    };
    assert_eq!(back.member.email, EMAIL);
    let view = members::get(&e.ctx, &Actor::local(), &e.moon)
        .await
        .unwrap();
    assert!(!view.dormant);
    assert_eq!(view.member.email, EMAIL);
    assert_eq!(count(&e, "SELECT count(*) FROM member_dormant").await, 0);
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM audit_log WHERE action = 'member.wake'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM members WHERE dormant_notice_at IS NOT NULL"
        )
        .await,
        0,
        "로그인하면 알림 표시도 지운다"
    );
}

#[tokio::test]
async fn dormant_member_wakes_through_reset_link() {
    let e = env().await;
    set(
        &e.ctx,
        SettingsPatch {
            dormant_days: Some(365),
            ..Default::default()
        },
    )
    .await;
    age_everyone(&e).await;
    run(&e).await;
    sql(&e, "UPDATE members SET dormant_notice_at = '2000-02-01T00:00:00Z' WHERE dormant_notice_at IS NOT NULL").await;
    assert_eq!(run(&e).await.dormant, 1);

    members::request_reset(&e.ctx, EMAIL).await.unwrap();
    let mail = mail::list(&e.ctx, &Actor::local(), Some(MailStatus::Queued), 20)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.subject.contains("비밀번호"))
        .unwrap();
    let raw = mail
        .body
        .split("/reset/")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let login = members::reset_password(&e.ctx, &raw, "new-password-22")
        .await
        .unwrap();
    assert_eq!(login.session().unwrap().member.email, EMAIL);
    let view = members::get(&e.ctx, &Actor::local(), &e.moon)
        .await
        .unwrap();
    assert!(!view.dormant);
    assert_eq!(
        view.member.email, EMAIL,
        "비밀번호를 바꾸며 주소를 덮어쓰지 않는다"
    );
}

#[tokio::test]
async fn staff_withdrawn_and_disabled_are_skipped() {
    let e = env().await;
    members::set_role(&e.ctx, &Actor::local(), &e.moon, "editor")
        .await
        .unwrap();
    set(
        &e.ctx,
        SettingsPatch {
            dormant_days: Some(365),
            ..Default::default()
        },
    )
    .await;
    age_everyone(&e).await;
    assert_eq!(run(&e).await.notified, 0, "운영진(편집자 이상)은 빠진다");

    members::set_role(&e.ctx, &Actor::local(), &e.moon, "member")
        .await
        .unwrap();
    sql(
        &e,
        "UPDATE members SET status = 'disabled' WHERE email = 'moon@example.test'",
    )
    .await;
    assert_eq!(run(&e).await.notified, 0, "멈춘 회원은 빠진다");
    sql(&e, "UPDATE members SET status = 'active', withdrawn_at = '2001-01-01T00:00:00Z' WHERE email = 'moon@example.test'").await;
    assert_eq!(run(&e).await.notified, 0, "탈퇴 회원은 빠진다");
}

#[tokio::test]
async fn old_sent_mail_is_removed() {
    let e = env().await;
    members::request_reset(&e.ctx, EMAIL).await.unwrap();
    members::request_reset(&e.ctx, "owner@example.test")
        .await
        .unwrap();
    assert_eq!(count(&e, "SELECT count(*) FROM mail_outbox").await, 2);
    // 하나는 오래전에 보냈고, 하나는 오래됐지만 아직 못 보냈다
    sql(
        &e,
        "UPDATE mail_outbox SET created_at = '2000-01-01T00:00:00Z'",
    )
    .await;
    sql(
        &e,
        "UPDATE mail_outbox SET status = 'sent' WHERE to_addr = 'moon@example.test'",
    )
    .await;
    let r = run(&e).await;
    assert_eq!(r.mails_removed, 1);
    assert_eq!(
        count(
            &e,
            "SELECT count(*) FROM mail_outbox WHERE status = 'queued'"
        )
        .await,
        1,
        "아직 못 보낸 편지는 남긴다"
    );
    assert_eq!(run(&e).await.mails_removed, 0);
}
