//! 2단계 인증 (GAP M16, ADR-0072): TOTP(RFC 6238)와 복구 코드.
//!
//! - HMAC-SHA1, 6자리, 30초. 앞뒤 한 순번까지 받고, 이미 받은 순번 이하는 받지 않는다(같은 코드 다시 쓰기 막기).
//! - 비밀번호·메일 링크로 들어온 회원이 TOTP 를 켰으면 세션 대신 5분짜리 확인 단계를 만든다. 틀리면 5번까지.
//! - 운영자 로그인 링크(`maru login-link`)는 거치지 않는다. 앱을 잃은 운영자가 돌아오는 길이다.
//! - 복구 코드는 10개, 1회용, SHA-256 해시만 둔다. 원문은 만들 때 한 번만 보여 준다.
//! - 감사에는 비밀·코드를 남기지 않는다.

use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use sha1::Sha1;

use crate::actor::{Actor, rank};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{Member, MemberStatus, MemberTotp, TotpChallenge};
use crate::services::auth::{self, NewSession, SignedIn};
use crate::services::{members, settings, tokens};
use crate::store::{Ctx, Tx};

pub const STEP_SECONDS: i64 = 30;
pub const DIGITS: usize = 6;
pub const CHALLENGE_MINUTES: i64 = 5;
pub const MAX_ATTEMPTS: i64 = 5;
pub const RECOVERY_CODES: usize = 10;
const SECRET_BYTES: usize = 20;
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// 내 2단계 인증 상태.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TotpStatus {
    pub enabled: bool,
    /// 설정 중이면 앱에 넣을 값
    pub setup: Option<Setup>,
    /// 쓰지 않은 복구 코드 수
    pub recovery_left: i64,
}

/// 인증 앱에 넣을 값.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Setup {
    /// base32 비밀 (앱에 직접 적을 때)
    pub secret: String,
    /// `otpauth://totp/...` (QR 코드·링크로 넣을 때)
    pub uri: String,
}

/// 켜기 결과. 다른 기기의 로그인은 끊기고 새 세션을 돌려준다.
#[derive(Debug, Clone)]
pub struct Enabled {
    pub session: NewSession,
    /// 원문. 다시 볼 수 없다.
    pub recovery_codes: Vec<String>,
}

/// 코드를 기다리는 로그인. `secret` 은 쿠키 값이고 다시 볼 수 없다.
#[derive(Debug, Clone)]
pub struct Pending {
    pub secret: String,
    pub expires_at: String,
    pub member: Member,
}

pub async fn status(ctx: &Ctx, who: &SignedIn) -> Result<TotpStatus> {
    let id = &who.member.id;
    Ok(match ctx.store.member_totp(id).await? {
        Some(t) if t.enabled_at.is_some() => TotpStatus {
            enabled: true,
            setup: None,
            recovery_left: ctx.store.recovery_codes_left(id).await?,
        },
        Some(t) => TotpStatus {
            enabled: false,
            setup: Some(setup_of(ctx, &who.member, &t.secret).await?),
            recovery_left: 0,
        },
        None => TotpStatus {
            enabled: false,
            setup: None,
            recovery_left: 0,
        },
    })
}

/// 설정을 시작한다. 이미 설정 중이면 같은 비밀을 다시 돌려준다(새로 고쳐도 앱에 넣은 값이 맞도록).
pub async fn start(ctx: &Ctx, who: &SignedIn) -> Result<Setup> {
    let mut tx = Tx::begin(ctx).await?;
    if let Some(t) = tx.member_totp(&who.member.id).await? {
        if t.enabled_at.is_some() {
            return Err(Error::new(Code::Conflict).with_message("error.totp_already"));
        }
        drop(tx);
        return setup_of(ctx, &who.member, &t.secret).await;
    }
    let secret = base32_encode(&random::<SECRET_BYTES>()?);
    tx.put_member_totp(&MemberTotp {
        member_id: who.member.id.clone(),
        secret: secret.clone(),
        enabled_at: None,
        last_step: 0,
        created_at: clock::now(),
    })
    .await?;
    tx.audit(
        &who.actor,
        "auth.totp_start",
        Target::new("member", &who.member.id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    setup_of(ctx, &who.member, &secret).await
}

/// 앱이 보여 주는 코드로 확인하고 켠다. 복구 코드를 새로 만들고, 다른 기기의 로그인은 끊는다.
pub async fn enable(ctx: &Ctx, who: &SignedIn, code: &str) -> Result<Enabled> {
    let mut tx = Tx::begin(ctx).await?;
    let mut t = tx
        .member_totp(&who.member.id)
        .await?
        .ok_or_else(|| Error::new(Code::Conflict).with_message("error.totp_not_started"))?;
    if t.enabled_at.is_some() {
        return Err(Error::new(Code::Conflict).with_message("error.totp_already"));
    }
    let step = digits(code)
        .and_then(|c| matching_step(&t.secret, &c, t.last_step, unix_now()))
        .ok_or_else(code_wrong)?;
    t.enabled_at = Some(clock::now());
    t.last_step = step;
    tx.put_member_totp(&t).await?;
    let codes = new_recovery_codes()?;
    tx.replace_recovery_codes(&t.member_id, &hashes(&codes))
        .await?;
    let member = tx
        .member_by_id(&who.member.id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(|| Error::new(Code::Unauthorized))?;
    tx.delete_member_sessions(&member.id).await?;
    tx.audit(
        &who.actor,
        "auth.totp_enable",
        Target::new("member", &member.id),
        json!({ "recovery_codes": codes.len() }),
    )
    .await?;
    let session = auth::start_session(&mut tx, member, "totp_enable").await?;
    tx.commit().await?;
    Ok(Enabled {
        session,
        recovery_codes: codes,
    })
}

/// 끈다. 켜져 있으면 지금 코드나 복구 코드가 있어야 하고, 설정 중이면 그냥 취소한다. 없으면 아무 일 없음.
pub async fn disable(ctx: &Ctx, who: &SignedIn, code: &str) -> Result<()> {
    let mut tx = Tx::begin(ctx).await?;
    let Some(t) = tx.member_totp(&who.member.id).await? else {
        return Ok(());
    };
    let was = if t.enabled_at.is_some() {
        second_factor(&mut tx, &t, code).await?;
        "enabled"
    } else {
        "setup"
    };
    tx.delete_member_totp(&t.member_id).await?;
    tx.audit(
        &who.actor,
        "auth.totp_disable",
        Target::new("member", &t.member_id),
        json!({ "was": was }),
    )
    .await?;
    tx.commit().await
}

/// 복구 코드를 새로 만든다(이전 것은 모두 무효). 지금 코드나 남은 복구 코드가 있어야 한다.
pub async fn regenerate(ctx: &Ctx, who: &SignedIn, code: &str) -> Result<Vec<String>> {
    let mut tx = Tx::begin(ctx).await?;
    let t = tx
        .member_totp(&who.member.id)
        .await?
        .filter(|t| t.enabled_at.is_some())
        .ok_or_else(|| Error::new(Code::Conflict).with_message("error.totp_off"))?;
    second_factor(&mut tx, &t, code).await?;
    let codes = new_recovery_codes()?;
    tx.replace_recovery_codes(&t.member_id, &hashes(&codes))
        .await?;
    tx.audit(
        &who.actor,
        "auth.totp_recovery",
        Target::new("member", &t.member_id),
        json!({ "recovery_codes": codes.len() }),
    )
    .await?;
    tx.commit().await?;
    Ok(codes)
}

/// 운영자가 회원의 2단계 인증을 끈다(앱과 복구 코드를 모두 잃었을 때). 꺼져 있었으면 `false`.
/// 로그인 링크와 같이 운영자(로컬 CLI, 사람 owner)만. 에이전트 토큰은 안 된다.
pub async fn reset(ctx: &Ctx, actor: &Actor, who: &str) -> Result<(Member, bool)> {
    auth::require_operator(actor)?;
    let member = members::find(ctx, who).await?;
    let mut tx = Tx::begin(ctx).await?;
    if !tx.delete_member_totp(&member.id).await? {
        return Ok((member, false));
    }
    tx.audit(
        actor,
        "auth.totp_reset",
        Target::new("member", &member.id),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok((member, true))
}

/// 2단계 인증을 켠 회원인지. 회원 목록·보기에 쓴다.
pub async fn is_enabled(ctx: &Ctx, member_id: &str) -> Result<bool> {
    Ok(ctx
        .store
        .member_totp(member_id)
        .await?
        .is_some_and(|t| t.enabled_at.is_some()))
}

/// 세션 대신 코드 확인 단계를 만든다 (`auth::finish_login`).
pub(crate) async fn challenge(tx: &mut Tx, member: &Member, method: &str) -> Result<Pending> {
    let raw = auth::new_secret()?;
    let c = TotpChallenge {
        id: clock::new_id(),
        token_hash: tokens::hash(&raw),
        member_id: member.id.clone(),
        method: method.to_owned(),
        attempts: 0,
        created_at: clock::now(),
        expires_at: clock::format(
            time::OffsetDateTime::now_utc() + time::Duration::minutes(CHALLENGE_MINUTES),
        ),
    };
    tx.insert_totp_challenge(&c).await?;
    tx.audit(
        &Actor::human(member.id.clone(), [], rank::GUEST),
        "auth.totp_challenge",
        Target::new("member", &member.id),
        json!({ "method": method }),
    )
    .await?;
    Ok(Pending {
        secret: raw,
        expires_at: c.expires_at,
        member: member.clone(),
    })
}

/// 확인 단계가 아직 쓸 수 있는지 (코드 입력 화면을 보이기 전).
pub async fn peek_challenge(ctx: &Ctx, raw: &str) -> Result<bool> {
    Ok(live_challenge(ctx, raw).await?.is_some())
}

/// 코드나 복구 코드를 받아 로그인을 끝낸다. 틀리면 남은 횟수를 알리고, 다 쓰면 확인 단계를 버린다.
pub async fn finish(ctx: &Ctx, raw: &str, code: &str) -> Result<NewSession> {
    let c = live_challenge(ctx, raw)
        .await?
        .ok_or_else(challenge_invalid)?;
    let mut tx = Tx::begin(ctx).await?;
    let member = tx
        .member_by_id(&c.member_id)
        .await?
        .filter(|m| m.status == MemberStatus::Active)
        .ok_or_else(challenge_invalid)?;
    let t = tx
        .member_totp(&member.id)
        .await?
        .filter(|t| t.enabled_at.is_some())
        .ok_or_else(challenge_invalid)?;
    match second_factor(&mut tx, &t, code).await {
        Ok(kind) => {
            if !tx.take_totp_challenge(&c.id).await? {
                return Err(challenge_invalid());
            }
            let method = format!("{}+{kind}", c.method);
            let session = auth::start_session(&mut tx, member, &method).await?;
            tx.commit().await?;
            Ok(session)
        }
        Err(e) if e.code() == Code::Unauthorized => {
            drop(tx); // 쓰기 잠금을 놓은 뒤 횟수를 센다
            let n = ctx.store.bump_totp_challenge(&c.id).await?;
            if n >= MAX_ATTEMPTS {
                ctx.store.delete_totp_challenge(&c.id).await?;
                return Err(Error::new(Code::Unauthorized).with_message("error.totp_locked"));
            }
            Err(Error::new(Code::Unauthorized)
                .with_message("error.totp_code_left")
                .arg("left", (MAX_ATTEMPTS - n).to_string()))
        }
        Err(e) => Err(e),
    }
}

async fn live_challenge(ctx: &Ctx, raw: &str) -> Result<Option<TotpChallenge>> {
    if raw.is_empty() || raw.len() > 128 {
        return Ok(None);
    }
    let now = clock::now();
    Ok(ctx
        .store
        .totp_challenge_by_hash(&tokens::hash(raw))
        .await?
        .filter(|c| c.expires_at > now && c.attempts < MAX_ATTEMPTS))
}

/// TOTP 코드면 순번을 올리고 `totp`, 복구 코드면 사용 처리하고 `recovery`. 틀리면 `error.totp_code`.
async fn second_factor(tx: &mut Tx, t: &MemberTotp, code: &str) -> Result<&'static str> {
    if let Some(d) = digits(code) {
        if let Some(step) = matching_step(&t.secret, &d, t.last_step, unix_now())
            && tx.advance_totp_step(&t.member_id, step).await?
        {
            return Ok("totp");
        }
        return Err(code_wrong());
    }
    let norm = normalize_recovery(code);
    if norm.len() == 10
        && tx
            .use_recovery_code(&t.member_id, &tokens::hash(&norm), &clock::now())
            .await?
    {
        return Ok("recovery");
    }
    Err(code_wrong())
}

async fn setup_of(ctx: &Ctx, member: &Member, secret: &str) -> Result<Setup> {
    let site = settings::site(ctx).await?;
    let issuer = if site.name.trim().is_empty() {
        "Maru".to_owned()
    } else {
        site.name.trim().to_owned()
    };
    let enc = |s: &str| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string();
    let uri = format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
        enc(&issuer),
        enc(&member.email),
        enc(&issuer),
    );
    Ok(Setup {
        secret: secret.to_owned(),
        uri,
    })
}

/// `unix` 초에 앱이 보여 줄 6자리 코드. 시험과 진단(`maru doctor` 류)에 쓴다. 비밀이 틀리면 `None`.
pub fn code_at(secret: &str, unix: i64) -> Option<String> {
    let step = u64::try_from(unix.div_euclid(STEP_SECONDS)).ok()?;
    hotp(&base32_decode(secret)?, step).map(|v| format!("{v:06}"))
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| Error::internal(e.to_string()))?;
    Ok(buf)
}

/// 공백을 뺀 6자리 숫자면 그것.
fn digits(code: &str) -> Option<String> {
    let d: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    (d.len() == DIGITS && d.bytes().all(|b| b.is_ascii_digit())).then_some(d)
}

/// `now` 의 앞뒤 한 순번 안에서 `code` 와 맞고 `last_step` 보다 큰 순번.
fn matching_step(secret: &str, code: &str, last_step: i64, now: i64) -> Option<i64> {
    let key = base32_decode(secret)?;
    let current = now.div_euclid(STEP_SECONDS);
    (current - 1..=current + 1)
        .filter(|s| *s > last_step && *s >= 0)
        .find(|s| {
            u64::try_from(*s)
                .ok()
                .and_then(|c| hotp(&key, c))
                .is_some_and(|v| auth::same_secret(&format!("{v:06}"), code))
        })
}

/// RFC 4226 HOTP, 6자리.
fn hotp(key: &[u8], counter: u64) -> Option<u32> {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).ok()?;
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = usize::from(h.last()? & 0x0f);
    let b = h.get(off..off + 4)?;
    let bin = u32::from_be_bytes([b[0] & 0x7f, b[1], b[2], b[3]]);
    Some(bin % 1_000_000)
}

fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = ((buf << 8) | u32::from(b)) & 0xffff;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(B32[((buf >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(B32[((buf << (5 - bits)) & 31) as usize]));
    }
    out
}

/// 대소문자·공백·`=` 는 무시한다. 다른 글자가 있으면 `None`.
fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let v = B32
            .iter()
            .position(|&b| char::from(b) == c.to_ascii_uppercase())?;
        buf = ((buf << 5) | v as u32) & 0xffff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// `abcde-fghij` 꼴 10개.
fn new_recovery_codes() -> Result<Vec<String>> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let raw = base32_encode(&random::<7>()?).to_lowercase();
            let (a, b) = (raw.get(0..5).unwrap_or(""), raw.get(5..10).unwrap_or(""));
            Ok(format!("{a}-{b}"))
        })
        .collect()
}

/// 소문자 영숫자만 남긴다 (`ABCDE-FGHIJ`, `abcde fghij` 모두 같은 코드).
fn normalize_recovery(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn hashes(codes: &[String]) -> Vec<String> {
    codes
        .iter()
        .map(|c| tokens::hash(&normalize_recovery(c)))
        .collect()
}

fn code_wrong() -> Error {
    Error::new(Code::Unauthorized).with_message("error.totp_code")
}

fn challenge_invalid() -> Error {
    Error::new(Code::Unauthorized).with_message("error.totp_expired")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 부록 B 의 SHA-1 시험값(8자리)의 끝 6자리.
    #[test]
    fn rfc6238_vectors() {
        let secret = base32_encode(b"12345678901234567890");
        for (t, want) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert!(matching_step(&secret, want, 0, t).is_some(), "t={t}");
        }
    }

    #[test]
    fn window_and_replay() {
        let secret = base32_encode(b"12345678901234567890");
        // 59초의 코드는 순번 1. 앞뒤 30초까지 받는다.
        assert_eq!(matching_step(&secret, "287082", 0, 59), Some(1));
        assert_eq!(matching_step(&secret, "287082", 0, 89), Some(1));
        assert_eq!(matching_step(&secret, "287082", 0, 29), Some(1));
        assert_eq!(matching_step(&secret, "287082", 0, 120), None);
        // 이미 받은 순번은 다시 받지 않는다.
        assert_eq!(matching_step(&secret, "287082", 1, 59), None);
        assert_eq!(matching_step(&secret, "000000", 0, 59), None);
    }

    #[test]
    fn base32_roundtrip() {
        assert_eq!(base32_encode(b"Hello!\xde\xad\xbe\xef"), "JBSWY3DPEHPK3PXP");
        assert_eq!(
            base32_decode("jbsw y3dp ehpk 3pxp").as_deref(),
            Some(&b"Hello!\xde\xad\xbe\xef"[..])
        );
        assert_eq!(base32_decode("not!base32"), None);
        let bytes = random::<SECRET_BYTES>().unwrap();
        assert_eq!(
            base32_decode(&base32_encode(&bytes)).as_deref(),
            Some(&bytes[..])
        );
    }

    #[test]
    fn recovery_codes_shape() {
        let codes = new_recovery_codes().unwrap();
        assert_eq!(codes.len(), RECOVERY_CODES);
        for c in &codes {
            assert_eq!(c.len(), 11);
            assert_eq!(normalize_recovery(&c.to_uppercase()).len(), 10);
        }
        assert_eq!(normalize_recovery("ABCDE-fghij"), "abcdefghij");
        assert_eq!(digits(" 123 456 ").as_deref(), Some("123456"));
        assert_eq!(digits("12345"), None);
    }
}
