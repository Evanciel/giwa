//! 2단계 인증 (GAP M16, ADR-0072). 실제 SQLite 파일로 켜기·로그인 확인 단계·다시 쓰기 막기·
//! 횟수 제한·복구 코드·운영자 해제·탈퇴 정리·비밀 없는 감사를 검증한다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use maru_core::actor::{Actor, Scope};
use maru_core::services::auth::{self, Login, SignedIn};
use maru_core::services::members::{self, SignupInput};
use maru_core::services::settings::{self, SettingsPatch};
use maru_core::services::{mail, totp};
use maru_core::{Code, Ctx};
use maru_storage::{DataDir, Db, FsBlobs, SqliteStore};

const PW: &str = "test-password-1";
const EMAIL: &str = "moon@example.test";

struct Env {
    _tmp: tempfile::TempDir,
    db: Db,
    ctx: Ctx,
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
        extensions: maru_core::store::no_extensions(),
    };
    auth::ensure_owner(&ctx, &Actor::local(), "owner@example.test", "운영자")
        .await
        .unwrap();
    settings::update(
        &ctx,
        &Actor::local(),
        SettingsPatch {
            name: Some("달빛 마을".into()),
            signup: Some("open".into()),
            signup_verify: Some(false),
            captcha: Some("off".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
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
    Env { _tmp: tmp, db, ctx }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

async fn who(ctx: &Ctx, secret: &str) -> SignedIn {
    auth::signed_in(ctx, secret)
        .await
        .unwrap()
        .expect("로그인 상태")
}

async fn login(ctx: &Ctx) -> Login {
    auth::login_password(ctx, EMAIL, PW).await.unwrap()
}

fn pending(l: Login) -> totp::Pending {
    match l {
        Login::Totp(p) => p,
        Login::Session(_) => panic!("코드 확인 단계여야 한다"),
    }
}

struct On {
    secret: String,
    codes: Vec<String>,
    session: String,
    /// 켤 때 쓴 시각. 그 순번은 다시 받지 않으니 다음 로그인은 `at + 30` 의 코드를 쓴다.
    at: i64,
}

impl On {
    fn next_code(&self) -> String {
        totp::code_at(&self.secret, self.at + 30).unwrap()
    }
}

async fn turn_on(e: &Env) -> On {
    let s = login(&e.ctx).await.session().unwrap();
    let me = who(&e.ctx, &s.secret).await;
    let setup = totp::start(&e.ctx, &me).await.unwrap();
    let at = now();
    let code = totp::code_at(&setup.secret, at).unwrap();
    let on = totp::enable(&e.ctx, &me, &code).await.unwrap();
    On {
        secret: setup.secret,
        codes: on.recovery_codes,
        session: on.session.secret,
        at,
    }
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(db.read_pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn setup_then_enable() {
    let e = env().await;
    let old = login(&e.ctx).await.session().unwrap();
    let me = who(&e.ctx, &old.secret).await;
    let st = totp::status(&e.ctx, &me).await.unwrap();
    assert!(!st.enabled && st.setup.is_none());

    let setup = totp::start(&e.ctx, &me).await.unwrap();
    assert_eq!(setup.secret.len(), 32);
    assert!(
        setup.uri.starts_with("otpauth://totp/")
            && setup.uri.contains(&format!("secret={}", setup.secret))
            && setup.uri.contains("moon%40example%2Etest"),
        "{}",
        setup.uri
    );
    // 다시 시작해도 같은 비밀 (앱에 넣은 값이 맞도록).
    assert_eq!(totp::start(&e.ctx, &me).await.unwrap().secret, setup.secret);
    let st = totp::status(&e.ctx, &me).await.unwrap();
    assert_eq!(st.setup.unwrap().secret, setup.secret);
    // 설정 중에는 로그인이 그대로다.
    assert!(matches!(login(&e.ctx).await, Login::Session(_)));

    let err = totp::enable(&e.ctx, &me, "000000").await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    assert!(!totp::status(&e.ctx, &me).await.unwrap().enabled);

    let code = totp::code_at(&setup.secret, now()).unwrap();
    let on = totp::enable(&e.ctx, &me, &code).await.unwrap();
    assert_eq!(on.recovery_codes.len(), 10);
    // 다른 세션은 끊기고 새 세션만 남는다.
    assert!(
        auth::signed_in(&e.ctx, &old.secret)
            .await
            .unwrap()
            .is_none()
    );
    let me = who(&e.ctx, &on.session.secret).await;
    let st = totp::status(&e.ctx, &me).await.unwrap();
    assert!(st.enabled && st.setup.is_none());
    assert_eq!(st.recovery_left, 10);
    assert_eq!(
        totp::start(&e.ctx, &me).await.unwrap_err().code(),
        Code::Conflict
    );
    let view = members::get(&e.ctx, &Actor::local(), EMAIL).await.unwrap();
    assert!(view.two_factor);

    // 복구 코드는 해시만, 감사에는 비밀·코드가 없다.
    assert_eq!(
        count(
            &e.db,
            "SELECT count(*) FROM member_recovery_codes WHERE length(code_hash) = 64"
        )
        .await,
        10
    );
    let diffs: Vec<String> = sqlx::query_scalar("SELECT diff FROM audit_log")
        .fetch_all(e.db.read_pool())
        .await
        .unwrap();
    let all = diffs.join("\n");
    assert!(!all.contains(&setup.secret));
    for c in &on.recovery_codes {
        assert!(!all.contains(c.as_str()));
    }
}

#[tokio::test]
async fn login_needs_code_once() {
    let e = env().await;
    let on = turn_on(&e).await;

    let p = pending(login(&e.ctx).await);
    // 확인 단계 값은 세션이 아니다.
    assert!(auth::signed_in(&e.ctx, &p.secret).await.unwrap().is_none());
    assert!(totp::peek_challenge(&e.ctx, &p.secret).await.unwrap());

    let err = totp::finish(&e.ctx, &p.secret, "000000").await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
    assert!(err.message().contains('4'), "{}", err.message());
    // 켤 때 쓴 순번은 다시 받지 않는다.
    let used = totp::code_at(&on.secret, on.at).unwrap();
    let next = on.next_code();
    if used != next {
        assert!(totp::finish(&e.ctx, &p.secret, &used).await.is_err());
    }
    let s = totp::finish(&e.ctx, &p.secret, &next).await.unwrap();
    assert_eq!(who(&e.ctx, &s.secret).await.member.email, EMAIL);
    // 확인 단계는 한 번만 쓴다.
    assert!(!totp::peek_challenge(&e.ctx, &p.secret).await.unwrap());
    assert!(totp::finish(&e.ctx, &p.secret, &next).await.is_err());
    let methods: Vec<String> = sqlx::query_scalar(
        "SELECT json_extract(diff, '$.method') FROM audit_log WHERE action = 'auth.login'",
    )
    .fetch_all(e.db.read_pool())
    .await
    .unwrap();
    assert!(methods.iter().any(|m| m == "password+totp"), "{methods:?}");
}

#[tokio::test]
async fn five_wrong_codes_end_the_challenge() {
    let e = env().await;
    let on = turn_on(&e).await;
    let p = pending(login(&e.ctx).await);
    for _ in 0..4 {
        totp::finish(&e.ctx, &p.secret, "abc").await.unwrap_err();
    }
    let err = totp::finish(&e.ctx, &p.secret, "111111").await.unwrap_err();
    assert!(err.message().contains("처음부터"), "{}", err.message());
    assert!(!totp::peek_challenge(&e.ctx, &p.secret).await.unwrap());
    let next = on.next_code();
    assert!(totp::finish(&e.ctx, &p.secret, &next).await.is_err());

    // 만료된 확인 단계도 쓸 수 없다.
    let p = pending(login(&e.ctx).await);
    sqlx::query("UPDATE totp_challenges SET expires_at = '2000-01-01T00:00:00Z'")
        .execute(e.db.write_pool())
        .await
        .unwrap();
    assert!(!totp::peek_challenge(&e.ctx, &p.secret).await.unwrap());
    assert!(totp::finish(&e.ctx, &p.secret, &next).await.is_err());
    auth::purge_expired(&e.ctx).await.unwrap();
    assert_eq!(
        count(&e.db, "SELECT count(*) FROM totp_challenges").await,
        0
    );
}

#[tokio::test]
async fn recovery_codes_work_once_and_regenerate() {
    let e = env().await;
    let On { codes, session, .. } = turn_on(&e).await;

    let p = pending(login(&e.ctx).await);
    let s = totp::finish(&e.ctx, &p.secret, &codes[0].to_uppercase())
        .await
        .unwrap();
    let me = who(&e.ctx, &s.secret).await;
    assert_eq!(totp::status(&e.ctx, &me).await.unwrap().recovery_left, 9);
    let p = pending(login(&e.ctx).await);
    assert!(totp::finish(&e.ctx, &p.secret, &codes[0]).await.is_err());

    // 새로 만들려면 코드가 있어야 하고, 이전 코드는 무효가 된다.
    let me = who(&e.ctx, &session).await;
    assert!(totp::regenerate(&e.ctx, &me, "000000").await.is_err());
    let fresh = totp::regenerate(&e.ctx, &me, &codes[1]).await.unwrap();
    assert_eq!(fresh.len(), 10);
    assert_eq!(totp::status(&e.ctx, &me).await.unwrap().recovery_left, 10);
    let p = pending(login(&e.ctx).await);
    assert!(totp::finish(&e.ctx, &p.secret, &codes[2]).await.is_err());
    totp::finish(&e.ctx, &p.secret, &fresh[0]).await.unwrap();

    // 끄려면 코드가 있어야 한다. 끄면 비밀번호만으로 들어온다.
    assert!(totp::disable(&e.ctx, &me, "").await.is_err());
    totp::disable(&e.ctx, &me, &fresh[1]).await.unwrap();
    assert!(matches!(login(&e.ctx).await, Login::Session(_)));
    assert_eq!(count(&e.db, "SELECT count(*) FROM member_totp").await, 0);
    assert_eq!(
        count(&e.db, "SELECT count(*) FROM member_recovery_codes").await,
        0
    );
    // 꺼진 것을 다시 꺼도 된다.
    totp::disable(&e.ctx, &me, "").await.unwrap();
}

#[tokio::test]
async fn reset_link_still_needs_code_but_login_link_does_not() {
    let e = env().await;
    let on = turn_on(&e).await;

    members::request_reset(&e.ctx, EMAIL).await.unwrap();
    let mails = mail::list(&e.ctx, &Actor::local(), None, 50).await.unwrap();
    let body = &mails
        .iter()
        .find(|m| m.to_addr == EMAIL && m.body.contains("/reset/"))
        .unwrap()
        .body;
    let at = body.find("/reset/").unwrap() + "/reset/".len();
    let raw = &body[at..at + 64];
    let p = pending(
        members::reset_password(&e.ctx, raw, "brand-new-password")
            .await
            .unwrap(),
    );
    // 비밀번호는 바뀌었지만 메일만으로는 들어오지 못한다.
    assert!(matches!(
        auth::login_password(&e.ctx, EMAIL, "brand-new-password")
            .await
            .unwrap(),
        Login::Totp(_)
    ));
    totp::finish(&e.ctx, &p.secret, &on.next_code())
        .await
        .unwrap();

    // 운영자 로그인 링크는 앱을 잃은 운영자가 돌아오는 길이라 거치지 않는다.
    let link = auth::issue_login_link(&e.ctx, &Actor::local(), Some(EMAIL))
        .await
        .unwrap();
    let raw = link.path.trim_start_matches("/admin/link/");
    assert!(auth::consume_link(&e.ctx, raw).await.is_ok());
}

#[tokio::test]
async fn operator_reset_and_withdraw_cleanup() {
    let e = env().await;
    turn_on(&e).await;

    let agent = Actor::agent("tok".into(), &[Scope::Admin], None);
    assert_eq!(
        totp::reset(&e.ctx, &agent, EMAIL).await.unwrap_err().code(),
        Code::Forbidden
    );
    let member = Actor::human("someone".into(), [], 0);
    assert!(totp::reset(&e.ctx, &member, EMAIL).await.is_err());
    let (_, removed) = totp::reset(&e.ctx, &Actor::local(), EMAIL).await.unwrap();
    assert!(removed);
    let (_, removed) = totp::reset(&e.ctx, &Actor::local(), EMAIL).await.unwrap();
    assert!(!removed);
    assert!(matches!(login(&e.ctx).await, Login::Session(_)));
    assert_eq!(
        count(
            &e.db,
            "SELECT count(*) FROM audit_log WHERE action = 'auth.totp_reset'"
        )
        .await,
        1
    );

    // 탈퇴하면 TOTP 와 복구 코드가 지워진다.
    turn_on(&e).await;
    let _waiting = pending(login(&e.ctx).await);
    let s = auth::consume_link(
        &e.ctx,
        auth::issue_login_link(&e.ctx, &Actor::local(), Some(EMAIL))
            .await
            .unwrap()
            .path
            .trim_start_matches("/admin/link/"),
    )
    .await
    .unwrap();
    let me = who(&e.ctx, &s.secret).await;
    members::withdraw(&e.ctx, &me, PW).await.unwrap();
    assert_eq!(count(&e.db, "SELECT count(*) FROM member_totp").await, 0);
    assert_eq!(
        count(&e.db, "SELECT count(*) FROM member_recovery_codes").await,
        0
    );
    assert_eq!(
        count(&e.db, "SELECT count(*) FROM totp_challenges").await,
        0
    );
}
