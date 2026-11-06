//! 소셜 로그인 모듈 (GAP M15, ADR-0114). 구글·카카오·네이버와 운영자가 주소를 넣는 OIDC 공급자로
//! 로그인·가입하고, 내 정보에서 연결을 더하거나 끊는다.
//!
//! - 흐름: `/m/social/<공급자>/start` → 공급자 → `/m/social/<공급자>/callback` → 연결된 회원이면 로그인,
//!   아니면 `/m/social/join` 에서 닉네임을 정해 가입. state 원문은 쿠키에, DB 에는 해시만 (ADR-0009).
//! - 이메일로 기존 회원에 자동 연결하는 것은 공급자가 주소를 확인했다고 알려 주고 그 확인을 믿을 때만
//!   (구글 `email_verified`, 카카오 `is_email_valid && is_email_verified`). 네이버·OIDC 는 믿지 않는다.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use maru_core::actor::{Actor, Permission, rank};
use maru_core::audit::{AuditRecord, Target};
use maru_core::model::{AuditEvent, MemberStatus, SignupMode};
use maru_core::services::{auth, members, settings, tokens};
use maru_core::store::ModuleMigration;
use maru_core::urls;
use maru_core::{Code, Ctx, Error, Result, clock};
use maru_modules::{
    LOGIN_OPTIONS, ME_SECTIONS, Manifest, Module, ModuleState, SlotContent, SlotView, ToolDef,
    module_error, schema_of,
};
use maru_storage::Db;
use maru_storage::fetch::{OAuthClient, check_oauth_url};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

static MANIFEST: Manifest = Manifest {
    id: "social",
    name: "소셜 로그인",
    version: env!("CARGO_PKG_VERSION"),
    description: "구글·카카오·네이버·OIDC 계정으로 로그인하고 가입합니다.",
    events: &["member.withdraw"],
};

/// 공급자 종류. 이 차례로 단추를 놓는다.
const PROVIDERS: [&str; 4] = ["google", "kakao", "naver", "oidc"];
const STATE_COOKIE: &str = "maru_social";
const JOIN_COOKIE: &str = "maru_social_join";
const STATE_MINUTES: i64 = 10;
const JOIN_MINUTES: i64 = 30;

#[derive(Clone)]
pub struct SocialModule {
    db: Db,
}

/// 저장한 공급자 설정.
#[derive(Debug, Clone)]
struct ProviderRow {
    id: String,
    client_id: String,
    client_secret: String,
    label: String,
    auth_url: String,
    token_url: String,
    userinfo_url: String,
    enabled: bool,
}

impl ProviderRow {
    fn label(&self) -> String {
        if !self.label.is_empty() {
            return self.label.clone();
        }
        match self.id.as_str() {
            "google" => "구글",
            "kakao" => "카카오",
            "naver" => "네이버",
            _ => "회사",
        }
        .to_owned()
    }

    /// 단추를 놓을 수 있는가 (켜졌고 클라이언트 ID·주소가 있다)
    fn usable(&self) -> bool {
        self.enabled
            && !self.client_id.is_empty()
            && (self.id != "oidc"
                || !(self.auth_url.is_empty()
                    || self.token_url.is_empty()
                    || self.userinfo_url.is_empty()))
    }
}

/// 공급자와 주고받는 주소와 방식.
struct Endpoints {
    auth: String,
    token: String,
    userinfo: String,
    scope: &'static str,
    /// PKCE S256 (구글·OIDC). 카카오·네이버 문서에는 없어 보내지 않는다.
    pkce: bool,
    nonce: bool,
}

/// 주소는 각 공급자의 공식 문서 (ADR-0114). 네이버는 공식 문서를 열지 못해 「공식 문서 미확인」이다.
fn endpoints(row: &ProviderRow) -> Endpoints {
    let fixed = |auth: &str, token: &str, userinfo: &str, scope, pkce| Endpoints {
        auth: auth.to_owned(),
        token: token.to_owned(),
        userinfo: userinfo.to_owned(),
        scope,
        pkce,
        nonce: pkce,
    };
    match row.id.as_str() {
        "google" => fixed(
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
            "openid email profile",
            true,
        ),
        "kakao" => fixed(
            "https://kauth.kakao.com/oauth/authorize",
            "https://kauth.kakao.com/oauth/token",
            "https://kapi.kakao.com/v2/user/me",
            "",
            false,
        ),
        "naver" => fixed(
            "https://nid.naver.com/oauth2.0/authorize",
            "https://nid.naver.com/oauth2.0/token",
            "https://openapi.naver.com/v1/nid/me",
            "",
            false,
        ),
        _ => fixed(
            &row.auth_url,
            &row.token_url,
            &row.userinfo_url,
            "openid email profile",
            true,
        ),
    }
}

/// 공급자가 알려 준 사람.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    subject: String,
    email: Option<String>,
    /// 공급자가 이 주소를 확인했고 우리가 그 확인을 믿는다
    trusted_email: bool,
    name: Option<String>,
}

fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(255).collect())
}

/// 사용자 정보 응답을 읽는다. 사용자 ID 가 없으면 `None`.
fn profile(provider: &str, v: &Value) -> Option<Profile> {
    let p = match provider {
        "kakao" => {
            let acc = &v["kakao_account"];
            let email = text(&acc["email"]);
            Profile {
                subject: v["id"]
                    .as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| text(&v["id"]))?,
                trusted_email: email.is_some()
                    && acc["is_email_valid"] == true
                    && acc["is_email_verified"] == true,
                email,
                name: text(&acc["profile"]["nickname"]),
            }
        }
        "naver" => {
            let r = &v["response"];
            Profile {
                subject: text(&r["id"])?,
                email: text(&r["email"]),
                trusted_email: false,
                name: text(&r["nickname"]).or_else(|| text(&r["name"])),
            }
        }
        _ => {
            let email = text(&v["email"]);
            Profile {
                subject: text(&v["sub"])?,
                trusted_email: provider == "google"
                    && email.is_some()
                    && v["email_verified"] == true,
                email,
                name: text(&v["name"]).or_else(|| text(&v["nickname"])),
            }
        }
    };
    Some(p)
}

/// RFC 4648 base64url, 채움 없음 (PKCE 의 code_challenge).
fn b64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        for i in 0..=chunk.len() {
            out.push(char::from(ABC[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

fn challenge(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn callback_url(ctx: &Ctx, provider: &str) -> String {
    format!(
        "{}/m/social/{provider}/callback",
        ctx.site_url.trim_end_matches('/')
    )
}

/// 사이트 안 주소만. 아니면 내 정보.
fn local_next(next: &str) -> String {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.chars().any(char::is_control);
    if ok { next.to_owned() } else { "/me".into() }
}

fn cookie(ctx: &Ctx, name: &str, value: &str, max_age: i64) -> HeaderValue {
    let mut c =
        format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/m/social; Max-Age={max_age}");
    if ctx.site_url.starts_with("https://") {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name && !v.is_empty()).then(|| v.to_owned())
        })
}

fn with_cookie(mut res: Response, value: HeaderValue) -> Response {
    res.headers_mut().append(header::SET_COOKIE, value);
    res
}

fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input).map_err(|e| {
        Error::new(Code::Validation)
            .with_message("error.json_body")
            .arg("detail", e.to_string())
    })
}

fn retry() -> Error {
    module_error(
        Code::Unauthorized,
        "로그인 확인 값이 맞지 않거나 시간이 지났습니다. 로그인 화면에서 처음부터 다시 시도하세요.",
    )
}

fn no_provider(id: &str) -> Error {
    module_error(
        Code::NotFound,
        format!("쓸 수 있는 로그인 공급자가 아닙니다: {id}"),
    )
    .next("maru social provider_list")
}

fn refused(what: &str, status: u16) -> Error {
    module_error(
        Code::Unauthorized,
        format!(
            "로그인 공급자가 {what}을 받아 주지 않았습니다 (응답 {status}). 잠시 뒤 다시 시도하세요."
        ),
    )
}

/// 공급자 설정 보기. 비밀값은 끝 네 글자만.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ProviderView {
    pub provider: String,
    pub label: String,
    pub client_id: String,
    pub secret_hint: String,
    pub enabled: bool,
    pub usable: bool,
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    /// 공급자 개발자 화면에 넣을 돌아올 주소
    pub redirect_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

fn view(ctx: &Ctx, row: &ProviderRow) -> ProviderView {
    let tail: String = {
        let chars: Vec<char> = row.client_secret.chars().collect();
        chars[chars.len().saturating_sub(4)..].iter().collect()
    };
    let ep = endpoints(row);
    ProviderView {
        provider: row.id.clone(),
        label: row.label(),
        client_id: row.client_id.clone(),
        secret_hint: if tail.is_empty() {
            String::new()
        } else {
            format!("…{tail}")
        },
        enabled: row.enabled,
        usable: row.usable(),
        auth_url: ep.auth,
        token_url: ep.token,
        userinfo_url: ep.userinfo,
        redirect_uri: callback_url(ctx, &row.id),
        note: (row.id == "naver").then_some(
            "네이버 주소는 공식 문서를 열지 못해 확인하지 못했습니다. 네이버 이메일로는 자동 연결하지 않습니다.",
        ),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProviderSetInput {
    /// google, kakao, naver, oidc
    provider: String,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    /// 단추 이름 (비우면 공급자 이름)
    #[serde(default)]
    label: Option<String>,
    /// oidc 만: 인증 주소
    #[serde(default)]
    auth_url: Option<String>,
    /// oidc 만: 토큰 주소
    #[serde(default)]
    token_url: Option<String>,
    /// oidc 만: 사용자 정보 주소
    #[serde(default)]
    userinfo_url: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProviderInput {
    provider: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdentitiesInput {
    /// 회원 ID. 비우면 나. 다른 회원은 회원 관리 권한이 있어야 한다
    #[serde(default)]
    member: Option<String>,
}

/// 회원에 연결된 공급자 계정.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IdentityRow {
    pub provider: String,
    pub email: String,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

impl SocialModule {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    async fn providers(&self) -> Result<Vec<ProviderRow>> {
        let rows = sqlx::query!(
            r#"SELECT id, client_id, client_secret, label, auth_url, token_url, userinfo_url,
                      enabled AS "enabled: bool"
               FROM mod_social_provider"#
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let mut out: Vec<ProviderRow> = rows
            .into_iter()
            .map(|r| ProviderRow {
                id: r.id,
                client_id: r.client_id,
                client_secret: r.client_secret,
                label: r.label,
                auth_url: r.auth_url,
                token_url: r.token_url,
                userinfo_url: r.userinfo_url,
                enabled: r.enabled,
            })
            .collect();
        out.sort_by_key(|r| PROVIDERS.iter().position(|p| *p == r.id));
        Ok(out)
    }

    async fn provider(&self, id: &str) -> Result<Option<ProviderRow>> {
        Ok(self.providers().await?.into_iter().find(|r| r.id == id))
    }

    async fn usable(&self, id: &str) -> Result<ProviderRow> {
        self.provider(id)
            .await?
            .filter(ProviderRow::usable)
            .ok_or_else(|| no_provider(id))
    }

    /// 진행 중인 흐름을 저장하고 원문을 돌려준다. 지난 흐름은 이때 지운다.
    async fn put_flow(
        &self,
        kind: &str,
        provider: &str,
        data: &Value,
        minutes: i64,
    ) -> Result<String> {
        let raw = auth::new_secret()?;
        let hash = tokens::hash(&raw);
        let now = clock::now();
        let expires = clock::minutes_later(minutes);
        let data = data.to_string();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!("DELETE FROM mod_social_flow WHERE expires_at < ?", now)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "INSERT INTO mod_social_flow (hash, kind, provider, data, expires_at) VALUES (?, ?, ?, ?, ?)",
            hash,
            kind,
            provider,
            data,
            expires
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(raw)
    }

    /// 흐름을 읽는다. `take` 면 지운다(한 번만 쓰게). 없거나 지났으면 `None`.
    async fn flow(&self, kind: &str, raw: &str, take: bool) -> Result<Option<(String, Value)>> {
        let hash = tokens::hash(raw);
        let row = sqlx::query!(
            "SELECT provider, data, expires_at FROM mod_social_flow WHERE hash = ? AND kind = ?",
            hash,
            kind
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        let Some(row) = row else {
            return Ok(None);
        };
        if take {
            let gone = sqlx::query!("DELETE FROM mod_social_flow WHERE hash = ?", hash)
                .execute(self.db.write_pool())
                .await
                .map_err(Error::internal)?
                .rows_affected();
            if gone == 0 {
                return Ok(None);
            }
        }
        if row.expires_at < clock::now() {
            return Ok(None);
        }
        let data = serde_json::from_str(&row.data).map_err(Error::internal)?;
        Ok(Some((row.provider, data)))
    }

    async fn identity_member(&self, provider: &str, subject: &str) -> Result<Option<String>> {
        sqlx::query_scalar!(
            "SELECT member_id FROM mod_social_identity WHERE provider = ? AND subject = ?",
            provider,
            subject
        )
        .fetch_optional(self.db.read_pool())
        .await
        .map_err(Error::internal)
    }

    async fn identities(&self, member_id: &str) -> Result<Vec<IdentityRow>> {
        let rows = sqlx::query_as!(
            IdentityRow,
            "SELECT provider, email, created_at, last_login_at FROM mod_social_identity
             WHERE member_id = ? ORDER BY created_at",
            member_id
        )
        .fetch_all(self.db.read_pool())
        .await
        .map_err(Error::internal)?;
        Ok(rows)
    }

    /// 공급자 계정을 회원에 잇는다.
    async fn link(
        &self,
        actor: &Actor,
        provider: &str,
        profile: &Profile,
        member_id: &str,
        auto: bool,
    ) -> Result<()> {
        let now = clock::now();
        let email = profile.email.clone().unwrap_or_default();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "INSERT INTO mod_social_identity (provider, subject, member_id, email, created_at, last_login_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            provider,
            profile.subject,
            member_id,
            email,
            now,
            now
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let audit = AuditRecord::new(
            actor,
            "social.link",
            Target::new("member", member_id),
            json!({ "provider": provider, "auto": auto }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    async fn touch(&self, provider: &str, subject: &str) -> Result<()> {
        let now = clock::now();
        sqlx::query!(
            "UPDATE mod_social_identity SET last_login_at = ? WHERE provider = ? AND subject = ?",
            now,
            provider,
            subject
        )
        .execute(self.db.write_pool())
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// 연결을 끊는다. 비밀번호도 다른 연결도 없으면 로그인할 길이 없어지므로 막는다.
    async fn unlink(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        member_id: &str,
        provider: &str,
    ) -> Result<()> {
        let member = ctx
            .store
            .member_by_id(member_id)
            .await?
            .ok_or_else(|| module_error(Code::NotFound, "회원을 찾을 수 없습니다."))?;
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let mine = sqlx::query!(
            "SELECT provider FROM mod_social_identity WHERE member_id = ?",
            member_id
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if !mine.iter().any(|r| r.provider == provider) {
            return Err(module_error(Code::NotFound, "연결된 계정이 아닙니다."));
        }
        if !member.has_password() && mine.len() <= 1 {
            return Err(module_error(
                Code::Conflict,
                "마지막 로그인 방법은 끊을 수 없습니다. 먼저 「비밀번호 찾기」로 비밀번호를 정하세요.",
            ));
        }
        sqlx::query!(
            "DELETE FROM mod_social_identity WHERE member_id = ? AND provider = ?",
            member_id,
            provider
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let audit = AuditRecord::new(
            actor,
            "social.unlink",
            Target::new("member", member_id),
            json!({ "provider": provider }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    /// 코드를 접근 토큰으로 바꾸고 사용자 정보를 읽는다.
    async fn exchange(
        &self,
        ctx: &Ctx,
        row: &ProviderRow,
        code: &str,
        state: &str,
        verifier: &str,
    ) -> Result<Profile> {
        let ep = endpoints(row);
        let client = OAuthClient::new().map_err(Error::internal)?;
        let redirect_uri = callback_url(ctx, &row.id);
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", row.client_id.as_str()),
        ];
        if !row.client_secret.is_empty() {
            form.push(("client_secret", row.client_secret.as_str()));
        }
        if ep.pkce {
            form.push(("code_verifier", verifier));
        }
        if row.id == "naver" {
            form.push(("state", state));
        }
        let unreachable = |e: String| {
            module_error(
                Code::Unauthorized,
                format!("로그인 공급자에 닿지 못했습니다: {e}"),
            )
        };
        let (status, token) = client
            .post_form(&ep.token, &form)
            .await
            .map_err(unreachable)?;
        let access = match token["access_token"].as_str() {
            Some(t) if status == 200 && !t.is_empty() => t.to_owned(),
            _ => return Err(refused("로그인 코드", status)),
        };
        let (status, info) = client
            .get_json(&ep.userinfo, &access)
            .await
            .map_err(unreachable)?;
        if status != 200 {
            return Err(refused("사용자 정보 요청", status));
        }
        profile(&row.id, &info).ok_or_else(|| refused("사용자 정보", status))
    }

    /// 공급자에서 돌아왔을 때. 로그인·연결·가입 화면 중 하나로 보낸다.
    async fn callback(
        &self,
        state: &ModuleState,
        provider: &str,
        headers: &HeaderMap,
        q: CallbackQuery,
    ) -> Result<Response> {
        let ctx = &state.ctx;
        if q.state.is_empty()
            || read_cookie(headers, STATE_COOKIE).as_deref() != Some(q.state.as_str())
        {
            return Err(retry());
        }
        let Some((flow_provider, data)) = self.flow("state", &q.state, true).await? else {
            return Err(retry());
        };
        if flow_provider != provider {
            return Err(retry());
        }
        if !q.error.is_empty() || q.code.is_empty() {
            return Err(module_error(
                Code::Unauthorized,
                "로그인 공급자에서 로그인을 마치지 않았습니다 (취소했거나 동의하지 않음).",
            ));
        }
        let row = self.usable(provider).await?;
        let verifier = data["verifier"].as_str().unwrap_or_default();
        let next = local_next(data["next"].as_str().unwrap_or_default());
        let profile = self
            .exchange(ctx, &row, &q.code, &q.state, verifier)
            .await?;
        let existing = self.identity_member(provider, &profile.subject).await?;
        let method = format!("social:{provider}");

        // 내 정보에서 연결하기
        if let Some(link) = data["link"].as_str() {
            let viewer = state.host.viewer(headers).await?;
            if viewer.member_id().map(String::as_str) != Some(link) {
                return Err(retry());
            }
            match existing {
                Some(m) if m == link => {}
                Some(_) => {
                    return Err(module_error(
                        Code::Conflict,
                        format!(
                            "이 {} 계정은 이미 다른 회원에 연결돼 있습니다.",
                            row.label()
                        ),
                    ));
                }
                // 공급자마다 한 계정만. 둘을 이으면 끊기 한 번에 둘 다 사라져 로그인할 길이 없어질 수 있다.
                None if self
                    .identities(link)
                    .await?
                    .iter()
                    .any(|i| i.provider == provider) =>
                {
                    return Err(module_error(
                        Code::Conflict,
                        format!(
                            "이미 다른 {} 계정이 연결돼 있습니다. 먼저 연결을 끊으세요.",
                            row.label()
                        ),
                    ));
                }
                None => self.link(&viewer, provider, &profile, link, false).await?,
            }
            return Ok(Redirect::to("/me#social").into_response());
        }

        if let Some(member) = existing {
            self.touch(provider, &profile.subject).await?;
            return Ok(state.host.sign_in(&member, &method, &next).await);
        }

        // 믿을 수 있는 확인된 주소가 메일 인증을 마친 회원과 같으면 그 회원에 잇는다.
        if let Some(email) = &profile.email
            && let Some(member) = ctx.store.member_by_email(&email.to_lowercase()).await?
        {
            let trusted = profile.trusted_email
                && member.status == MemberStatus::Active
                && member.email_verified_at.is_some();
            if !trusted {
                return Err(module_error(
                    Code::Conflict,
                    "이미 가입된 이메일입니다. 그 계정으로 로그인한 뒤 내 정보에서 소셜 로그인을 연결하세요.",
                ));
            }
            let actor = Actor::human(member.id.clone(), [], rank::GUEST);
            self.link(&actor, provider, &profile, &member.id, true)
                .await?;
            return Ok(state.host.sign_in(&member.id, &method, &next).await);
        }

        if settings::site(ctx).await?.signup != SignupMode::Open {
            return Err(module_error(
                Code::Forbidden,
                "이 사이트는 지금 새 회원을 받지 않습니다. 이미 회원이면 로그인한 뒤 내 정보에서 연결하세요.",
            ));
        }
        let join = json!({
            "subject": profile.subject,
            "email": profile.email,
            "trusted": profile.trusted_email,
            "name": profile.name,
            "next": next,
        });
        let raw = self.put_flow("join", provider, &join, JOIN_MINUTES).await?;
        Ok(with_cookie(
            Redirect::to("/m/social/join").into_response(),
            cookie(ctx, JOIN_COOKIE, &raw, JOIN_MINUTES * 60),
        ))
    }

    async fn tool_provider_set(
        &self,
        ctx: &Ctx,
        actor: &Actor,
        input: ProviderSetInput,
    ) -> Result<Value> {
        let id = input.provider.trim();
        if !PROVIDERS.contains(&id) {
            return Err(module_error(
                Code::Validation,
                "공급자는 google, kakao, naver, oidc 중 하나입니다.",
            ));
        }
        let old = self.provider(id).await?;
        let pick = |new: Option<String>, old: Option<&str>| {
            new.map(|v| v.trim().to_owned())
                .or_else(|| old.map(str::to_owned))
                .unwrap_or_default()
        };
        let client_id = pick(input.client_id, old.as_ref().map(|o| o.client_id.as_str()));
        let client_secret = pick(
            input.client_secret,
            old.as_ref().map(|o| o.client_secret.as_str()),
        );
        let label = pick(input.label, old.as_ref().map(|o| o.label.as_str()));
        let auth_url = pick(input.auth_url, old.as_ref().map(|o| o.auth_url.as_str()));
        let token_url = pick(input.token_url, old.as_ref().map(|o| o.token_url.as_str()));
        let userinfo_url = pick(
            input.userinfo_url,
            old.as_ref().map(|o| o.userinfo_url.as_str()),
        );
        let enabled = input
            .enabled
            .or(old.as_ref().map(|o| o.enabled))
            .unwrap_or(true);
        if client_id.is_empty() || client_id.len() > 512 || client_secret.len() > 512 {
            return Err(module_error(
                Code::Validation,
                "클라이언트 ID 를 넣으세요 (512자 이하, 비밀값도 512자 이하).",
            )
            .next(format!(
                "maru social provider_set --provider {id} --client_id <ID> --client_secret <비밀값>"
            )));
        }
        if label.chars().count() > 20 {
            return Err(module_error(
                Code::Validation,
                "단추 이름은 20자 이하입니다.",
            ));
        }
        let urls = [&auth_url, &token_url, &userinfo_url];
        if id == "oidc" {
            for u in urls {
                check_oauth_url(u).map_err(|e| {
                    module_error(Code::Validation, format!("oidc 주소를 쓸 수 없습니다: {e}"))
                        .next("maru social provider_set --provider oidc --auth_url https://… --token_url https://… --userinfo_url https://…")
                })?;
            }
        } else if urls.iter().any(|u| !u.is_empty()) {
            return Err(module_error(
                Code::Validation,
                "주소는 oidc 공급자만 정합니다. 구글·카카오·네이버 주소는 정해져 있습니다.",
            ));
        }
        let now = clock::now();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        sqlx::query!(
            "INSERT INTO mod_social_provider
               (id, client_id, client_secret, label, auth_url, token_url, userinfo_url, enabled, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET client_id = excluded.client_id,
               client_secret = excluded.client_secret, label = excluded.label,
               auth_url = excluded.auth_url, token_url = excluded.token_url,
               userinfo_url = excluded.userinfo_url, enabled = excluded.enabled,
               updated_at = excluded.updated_at",
            id,
            client_id,
            client_secret,
            label,
            auth_url,
            token_url,
            userinfo_url,
            enabled,
            now
        )
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let audit = AuditRecord::new(
            actor,
            "social.provider_set",
            Target::new("social_provider", id),
            json!({ "enabled": enabled, "secret_changed": old.as_ref().is_none_or(|o| o.client_secret != client_secret) }),
        );
        maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        tx.commit().await.map_err(Error::internal)?;
        let row = self.provider(id).await?.ok_or_else(|| no_provider(id))?;
        Ok(json!(view(ctx, &row)))
    }

    async fn tool_provider_remove(&self, actor: &Actor, input: ProviderInput) -> Result<Value> {
        let id = input.provider.trim();
        let mut tx = self
            .db
            .write_pool()
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(Error::internal)?;
        let gone = sqlx::query!("DELETE FROM mod_social_provider WHERE id = ?", id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if gone > 0 {
            let audit = AuditRecord::new(
                actor,
                "social.provider_remove",
                Target::new("social_provider", id),
                json!({}),
            );
            maru_storage::sqlite::insert_audit(&mut tx, &audit).await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        // 연결은 남긴다: 같은 공급자를 다시 넣으면 그대로 로그인된다.
        Ok(json!({ "removed": gone > 0 }))
    }

    async fn tool_identities(&self, actor: &Actor, input: IdentitiesInput) -> Result<Value> {
        let member = match input.member {
            Some(m) if actor.member_id() != Some(&m) => {
                actor.require(Permission::MemberManage)?;
                m
            }
            Some(m) => m,
            None => actor.member_id().cloned().ok_or_else(|| {
                module_error(Code::Validation, "회원을 적으세요: --member <회원 ID>")
            })?,
        };
        Ok(json!({ "items": self.identities(&member).await? }))
    }
}

#[async_trait]
impl Module for SocialModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 20261106400001,
            name: "social",
            up: include_str!("../migrations/20261106400001_social.up.sql"),
            down: include_str!("../migrations/20261106400001_social.down.sql"),
        }]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/join", get(join_page).post(join_form))
                .route("/unlink/{provider}", post(unlink_form))
                .route("/{provider}/start", get(start))
                .route("/{provider}/callback", get(callback))
                .layer(Extension(Arc::new(self.clone()))),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "mod/social/login.html",
                include_str!("../templates/login.html"),
            ),
            ("mod/social/me.html", include_str!("../templates/me.html")),
            (
                "mod/social/join.html",
                include_str!("../templates/join.html"),
            ),
        ]
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "provider_set",
                description: "로그인 공급자를 넣거나 고친다 (클라이언트 ID·비밀값, oidc 는 주소도)",
                input_schema: schema_of::<ProviderSetInput>(),
                permission: Some(Permission::SiteAdmin),
            },
            ToolDef {
                name: "provider_list",
                description: "로그인 공급자 설정과 공급자에 넣을 돌아올 주소 (비밀값은 끝 네 글자만)",
                input_schema: schema_of::<Empty>(),
                permission: Some(Permission::SiteAdmin),
            },
            ToolDef {
                name: "provider_remove",
                description: "로그인 공급자를 뺀다 (회원 연결은 남는다)",
                input_schema: schema_of::<ProviderInput>(),
                permission: Some(Permission::SiteAdmin),
            },
            ToolDef {
                name: "identities",
                description: "회원에 연결된 소셜 계정",
                input_schema: schema_of::<IdentitiesInput>(),
                permission: None,
            },
        ]
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        match tool {
            "provider_set" => {
                self.tool_provider_set(ctx, actor, parse_input(input)?)
                    .await
            }
            "provider_list" => {
                let _: Empty = parse_input(input)?;
                let rows = self.providers().await?;
                Ok(json!({ "items": rows.iter().map(|r| view(ctx, r)).collect::<Vec<_>>() }))
            }
            "provider_remove" => self.tool_provider_remove(actor, parse_input(input)?).await,
            "identities" => self.tool_identities(actor, parse_input(input)?).await,
            other => Err(maru_modules::tool_not_found(MANIFEST.id, other)),
        }
    }

    /// 탈퇴한 회원의 연결은 남기지 않는다 (같은 계정으로 새로 가입할 수 있게).
    async fn on_event(&self, _ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        if ev.action == "member.withdraw"
            && let Some(member) = &ev.target_id
        {
            sqlx::query!(
                "DELETE FROM mod_social_identity WHERE member_id = ?",
                member
            )
            .execute(self.db.write_pool())
            .await
            .map_err(Error::internal)?;
        }
        Ok(())
    }

    async fn purge(&self, _ctx: &Ctx) -> Result<u64> {
        let now = clock::now();
        let n = sqlx::query!("DELETE FROM mod_social_flow WHERE expires_at < ?", now)
            .execute(self.db.write_pool())
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n)
    }

    async fn slot(
        &self,
        _ctx: &Ctx,
        slot: &str,
        view: &SlotView<'_>,
    ) -> Result<Option<SlotContent>> {
        match slot {
            LOGIN_OPTIONS => {
                let buttons: Vec<Value> = self
                    .providers()
                    .await?
                    .iter()
                    .filter(|r| r.usable())
                    .map(|r| json!({ "id": r.id, "label": r.label(), "url": format!("/m/social/{}/start", r.id) }))
                    .collect();
                if buttons.is_empty() {
                    return Ok(None);
                }
                Ok(Some(SlotContent {
                    template: "mod/social/login.html",
                    data: json!({ "providers": buttons }),
                }))
            }
            ME_SECTIONS => {
                let Some(member) = view.viewer.member_id() else {
                    return Ok(None);
                };
                let linked = self.identities(member).await?;
                let providers = self.providers().await?;
                let mut rows = Vec::new();
                for id in PROVIDERS {
                    let row = providers.iter().find(|r| r.id == id);
                    let link = linked.iter().find(|l| l.provider == id);
                    let usable = row.is_some_and(ProviderRow::usable);
                    if !usable && link.is_none() {
                        continue;
                    }
                    rows.push(json!({
                        "id": id,
                        "label": row.map(ProviderRow::label).unwrap_or_else(|| id.to_owned()),
                        "linked": link.map(|l| json!({ "email": l.email, "date": l.created_at.get(..10).unwrap_or_default() })),
                        "link_url": usable.then(|| format!("/m/social/{id}/start?link=1")),
                        "unlink_action": format!("/m/social/unlink/{id}"),
                    }));
                }
                if rows.is_empty() {
                    return Ok(None);
                }
                Ok(Some(SlotContent {
                    template: "mod/social/me.html",
                    data: json!({ "providers": rows }),
                }))
            }
            _ => Ok(None),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct StartQuery {
    #[serde(default)]
    next: String,
    #[serde(default)]
    link: Option<String>,
}

async fn start(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<SocialModule>>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
    headers: HeaderMap,
) -> Response {
    let row = match module.usable(&provider).await {
        Ok(r) => r,
        Err(e) => return state.host.error(&e).await,
    };
    let link = if q.link.is_some() {
        let actor = match state.host.viewer(&headers).await {
            Ok(a) => a,
            Err(e) => return state.host.error(&e).await,
        };
        match actor.member_id() {
            Some(m) => Some(m.clone()),
            None => {
                return Redirect::to(&urls::with_query("/login", &[("next", "/me")]))
                    .into_response();
            }
        }
    } else {
        None
    };
    let (verifier, nonce) = match (auth::new_secret(), auth::new_secret()) {
        (Ok(v), Ok(n)) => (v, n),
        (Err(e), _) | (_, Err(e)) => return state.host.error(&e).await,
    };
    let data = json!({ "verifier": verifier, "next": local_next(&q.next), "link": link });
    let raw = match module
        .put_flow("state", &provider, &data, STATE_MINUTES)
        .await
    {
        Ok(r) => r,
        Err(e) => return state.host.error(&e).await,
    };
    let ep = endpoints(&row);
    let redirect_uri = callback_url(&state.ctx, &provider);
    let code_challenge = challenge(&verifier);
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", row.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("state", raw.as_str()),
        ("scope", ep.scope),
    ];
    if ep.pkce {
        params.push(("code_challenge", code_challenge.as_str()));
        params.push(("code_challenge_method", "S256"));
    }
    if ep.nonce {
        params.push(("nonce", nonce.as_str()));
    }
    with_cookie(
        Redirect::to(&urls::with_query(&ep.auth, &params)).into_response(),
        cookie(&state.ctx, STATE_COOKIE, &raw, STATE_MINUTES * 60),
    )
}

#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: String,
}

async fn callback(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<SocialModule>>,
    Path(provider): Path<String>,
    Query(q): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let res = match module.callback(&state, &provider, &headers, q).await {
        Ok(r) => r,
        Err(e) => state.host.error(&e).await,
    };
    with_cookie(res, cookie(&state.ctx, STATE_COOKIE, "", 0))
}

/// 가입 화면에 넘기는 값.
fn join_view(
    provider: &str,
    data: &Value,
    email: &str,
    name: &str,
    error: Option<String>,
) -> Value {
    let label = match provider {
        "google" => "구글",
        "kakao" => "카카오",
        "naver" => "네이버",
        _ => "외부",
    };
    json!({
        "provider_label": label,
        "email": email,
        "display_name": name,
        "from_provider": data["email"].as_str().is_some(),
        "error": error,
        "noindex": true,
    })
}

async fn join_page(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<SocialModule>>,
    headers: HeaderMap,
) -> Response {
    let Some(raw) = read_cookie(&headers, JOIN_COOKIE) else {
        return state.host.error(&retry()).await;
    };
    match module.flow("join", &raw, false).await {
        Ok(Some((provider, data))) => {
            let email = data["email"].as_str().unwrap_or_default();
            let name = data["name"].as_str().unwrap_or_default();
            let v = join_view(&provider, &data, email, name, None);
            state
                .host
                .page("mod/social/join.html", "소셜 계정으로 가입", v)
                .await
        }
        Ok(None) => state.host.error(&retry()).await,
        Err(e) => state.host.error(&e).await,
    }
}

#[derive(Debug, Default, Deserialize)]
struct JoinForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: String,
}

async fn join_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<SocialModule>>,
    headers: HeaderMap,
    Form(f): Form<JoinForm>,
) -> Response {
    if !state.host.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(raw) = read_cookie(&headers, JOIN_COOKIE) else {
        return state.host.error(&retry()).await;
    };
    let (provider, data) = match module.flow("join", &raw, false).await {
        Ok(Some(v)) => v,
        Ok(None) => return state.host.error(&retry()).await,
        Err(e) => return state.host.error(&e).await,
    };
    let given = data["email"].as_str().unwrap_or_default();
    // 공급자가 확인해 준 주소를 그대로 쓸 때만 메일 인증을 건너뛴다.
    let verified = data["trusted"] == true && f.email.trim().eq_ignore_ascii_case(given);
    let method = format!("social:{provider}");
    let input = members::ExternalSignupInput {
        email: f.email.clone(),
        display_name: f.display_name.clone(),
        email_verified: verified,
        method: method.clone(),
    };
    let done = match members::signup_external(&state.ctx, input).await {
        Ok(d) => d,
        Err(e) => {
            let v = join_view(
                &provider,
                &data,
                &f.email,
                &f.display_name,
                Some(e.message()),
            );
            return state
                .host
                .page("mod/social/join.html", "소셜 계정으로 가입", v)
                .await;
        }
    };
    let profile = Profile {
        subject: data["subject"].as_str().unwrap_or_default().to_owned(),
        email: Some(f.email.trim().to_lowercase()),
        trusted_email: verified,
        name: None,
    };
    let actor = Actor::human(done.member_id.clone(), [], rank::GUEST);
    let linked = async {
        module.flow("join", &raw, true).await?;
        module
            .link(&actor, &provider, &profile, &done.member_id, false)
            .await
    }
    .await;
    if let Err(e) = linked {
        return state.host.error(&e).await;
    }
    let res = if done.verify_required {
        state
            .host
            .page(
                "mod/social/join.html",
                "메일을 확인하세요",
                json!({ "sent": true, "email": f.email.trim(), "noindex": true }),
            )
            .await
    } else {
        let next = local_next(data["next"].as_str().unwrap_or_default());
        state.host.sign_in(&done.member_id, &method, &next).await
    };
    with_cookie(res, cookie(&state.ctx, JOIN_COOKIE, "", 0))
}

async fn unlink_form(
    State(state): State<ModuleState>,
    Extension(module): Extension<Arc<SocialModule>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !state.host.same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let actor = match state.host.viewer(&headers).await {
        Ok(a) => a,
        Err(e) => return state.host.error(&e).await,
    };
    let Some(member) = actor.member_id() else {
        return Redirect::to(&urls::with_query("/login", &[("next", "/me")])).into_response();
    };
    match module.unlink(&state.ctx, &actor, member, &provider).await {
        Ok(()) => Redirect::to("/me#social").into_response(),
        Err(e) => state.host.error(&e).await,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636() {
        // RFC 7636 부록 B
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
    }

    #[test]
    fn only_verified_google_and_kakao_emails_are_trusted() {
        let g = profile(
            "google",
            &json!({ "sub": "1", "email": "a@example.test", "email_verified": true }),
        )
        .unwrap();
        assert!(g.trusted_email);
        let g = profile(
            "google",
            &json!({ "sub": "1", "email": "a@example.test", "email_verified": false }),
        )
        .unwrap();
        assert!(!g.trusted_email);
        let o = profile(
            "oidc",
            &json!({ "sub": "1", "email": "a@example.test", "email_verified": true }),
        )
        .unwrap();
        assert!(!o.trusted_email);
        let k = profile(
            "kakao",
            &json!({ "id": 42, "kakao_account": { "email": "a@example.test",
                "is_email_valid": true, "is_email_verified": true, "profile": { "nickname": "카카" } } }),
        )
        .unwrap();
        assert_eq!((k.subject.as_str(), k.trusted_email), ("42", true));
        assert_eq!(k.name.as_deref(), Some("카카"));
        let k = profile(
            "kakao",
            &json!({ "id": 42, "kakao_account": { "email": "a@example.test",
                "is_email_valid": false, "is_email_verified": true } }),
        )
        .unwrap();
        assert!(!k.trusted_email);
        let n = profile(
            "naver",
            &json!({ "resultcode": "00", "response": { "id": "abc", "email": "a@example.test" } }),
        )
        .unwrap();
        assert_eq!((n.subject.as_str(), n.trusted_email), ("abc", false));
        assert!(profile("google", &json!({ "email": "a@example.test" })).is_none());
    }

    #[test]
    fn next_stays_on_site() {
        assert_eq!(local_next("/b/free"), "/b/free");
        assert_eq!(local_next("//evil.example"), "/me");
        assert_eq!(local_next("https://evil.example"), "/me");
        assert_eq!(local_next("/\\evil"), "/me");
        assert_eq!(local_next(""), "/me");
    }
}
