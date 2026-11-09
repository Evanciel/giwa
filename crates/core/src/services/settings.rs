//! 사이트 설정. `site_settings` 에 `site.<필드>` 키로 하나씩 저장한다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{CaptchaMode, SKINS, SignupMode, SiteSettings};
use crate::services::{moderation, required_text, retention, signals};
use crate::store::{Ctx, Tx};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub tz_offset_min: Option<i32>,
    #[serde(default)]
    pub posts_per_page: Option<u32>,
    #[serde(default)]
    pub naver_verification: Option<String>,
    #[serde(default)]
    pub google_verification: Option<String>,
    /// 첫 화면으로 쓸 페이지 주소. 빈 값이면 최신 글 목록 (ADR-0125)
    #[serde(default)]
    pub home_page: Option<String>,
    /// 목차를 보일 최소 제목 수 (0 = 끔, 최대 20)
    #[serde(default)]
    pub toc_min_headings: Option<u32>,
    /// 공개 화면에 조회수를 보일지 (세기는 늘 한다)
    #[serde(default)]
    pub show_views: Option<bool>,
    /// 공개 화면 머리글에 검색 칸을 보일지
    #[serde(default)]
    pub show_search: Option<bool>,
    /// 글 아래에 게시판 목록을 보일지 (GAP Q19)
    #[serde(default)]
    pub list_under_post: Option<bool>,
    /// 표 목록 왼쪽 칸에 번호 대신 추천 수를 보일지 (GAP Q29)
    #[serde(default)]
    pub list_votes: Option<bool>,
    /// 댓글·공개 글쓰기의 스티커를 쓸지 (GAP E15)
    #[serde(default)]
    pub stickers: Option<bool>,
    /// 회원 가입: off · open (회원 관리 권한 필요)
    #[serde(default)]
    pub signup: Option<String>,
    /// 가입한 사람이 메일 링크를 눌러야 로그인할지 (회원 관리 권한 필요)
    #[serde(default)]
    pub signup_verify: Option<bool>,
    /// 사람 확인 질문: off · guest (회원 관리 권한 필요)
    #[serde(default)]
    pub captcha: Option<String>,
    /// 방문자 통계를 며칠 남길지 (30~3650)
    #[serde(default)]
    pub stats_retention_days: Option<u32>,
    /// 금지어. 줄바꿈이나 쉼표로 나눈다. 빈 글이면 모두 지운다
    pub banned_words: Option<String>,
    /// 회원이 글·댓글을 쓰는 최소 간격(초, 0~3600). 0 이면 끔
    pub flood_seconds: Option<u32>,
    /// 휴면으로 바꿀 미접속 날수 (0 = 끔, 180~3650). 회원 관리 권한 필요
    #[serde(default)]
    pub dormant_days: Option<u32>,
    /// 보냈거나 실패한 편지를 남길 날수 (7~3650). 회원 관리 권한 필요
    #[serde(default)]
    pub mail_retention_days: Option<u32>,
    /// 한 글에 지난 1시간 댓글이 이만큼이면 운영 신호 (0 = 끔, 3~1000)
    #[serde(default)]
    pub signal_comment_burst: Option<u32>,
    /// 새 운영 신호를 관리자·소유자에게 메일로
    #[serde(default)]
    pub signal_mail: Option<bool>,
    /// 링크 카드 정보를 서버가 가져온다 (ADR-0084)
    #[serde(default)]
    pub link_cards: Option<bool>,
    /// 공개 화면 스킨: default · community (ADR-0081)
    #[serde(default)]
    pub skin: Option<String>,
    /// 공유 미리보기 그림 주소: `/media/…` 또는 `https://…`. 빈 값이면 서버가 그린 그림 (ADR-0143)
    #[serde(default)]
    pub share_image: Option<String>,
    /// 공개한 사이트의 새 글 주소를 IndexNow 로 검색엔진에 바로 알릴지 (ADR-0143)
    #[serde(default)]
    pub indexnow: Option<bool>,
}

/// 공유 그림 주소로 쓸 수 있는가. 사이트 미디어 경로나 https 주소, 공백·따옴표·꺾쇠 없이 500자까지.
pub fn share_image_ok(u: &str) -> bool {
    let clean = u.len() <= 500
        && !u.contains("..")
        && !u
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\'));
    let shaped = u
        .strip_prefix("/media/")
        .is_some_and(|rest| !rest.is_empty())
        || u.strip_prefix("https://")
            .is_some_and(|rest| rest.split('/').next().is_some_and(|h| h.contains('.')));
    clean && shaped
}

/// 저장된 값 위에 기본값을 채운다. 모르는 키나 잘못된 값은 기본값으로 둔다.
pub async fn site(ctx: &Ctx) -> Result<SiteSettings> {
    let s = from_pairs(ctx.store.settings().await?);
    // 오류·편지 문장은 `Ctx` 없이 만들어지므로 읽을 때마다 프로세스 언어를 맞춘다 (ADR-0134).
    // 다른 프로세스(CLI)가 언어를 바꿔도 서버의 다음 요청부터 따라간다.
    crate::messages::set_lang(&s.lang);
    Ok(s)
}

fn from_pairs(pairs: Vec<(String, Value)>) -> SiteSettings {
    let mut map = serde_json::to_value(SiteSettings::default())
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (key, value) in pairs {
        if let Some(field) = key.strip_prefix("site.")
            && map.contains_key(field)
        {
            map.insert(field.to_owned(), value);
        }
    }
    serde_json::from_value(Value::Object(map)).unwrap_or_default()
}

/// 사이트 이름: 비울 수 없고 80자까지. 화면 편집기 초안(ADR-0145)도 같은 검사를 쓴다.
pub fn check_name(name: &str) -> Result<String> {
    required_text("name", name, 80)
}

/// 사이트 설명: 비워도 되고 300자까지.
pub fn check_description(description: &str) -> Result<String> {
    let d = description.trim();
    if d.chars().count() > 300 {
        return Err(Error::new(Code::Validation)
            .with_message("error.field_length")
            .arg("field", "description")
            .arg("max", "300"));
    }
    Ok(d.to_owned())
}

pub async fn update(ctx: &Ctx, actor: &Actor, patch: SettingsPatch) -> Result<SiteSettings> {
    actor.require(Permission::StructureEdit)?;
    let mut changes: Vec<(&'static str, Value)> = Vec::new();
    if let Some(n) = &patch.name {
        changes.push(("name", json!(check_name(n)?)));
    }
    if let Some(d) = &patch.description {
        changes.push(("description", json!(check_description(d)?)));
    }
    if let Some(l) = &patch.lang {
        let ok = !l.is_empty()
            && l.len() <= 20
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok {
            return Err(Error::new(Code::Validation)
                .with_message("error.setting_lang")
                .arg("value", l.clone()));
        }
        changes.push(("lang", json!(l)));
    }
    if let Some(tz) = patch.tz_offset_min {
        if !(-720..=840).contains(&tz) {
            return Err(Error::new(Code::Validation)
                .with_message("error.setting_tz")
                .arg("value", tz.to_string()));
        }
        changes.push(("tz_offset_min", json!(tz)));
    }
    if let Some(n) = patch.posts_per_page {
        changes.push(("posts_per_page", json!(n.clamp(1, 100))));
    }
    if let Some(h) = &patch.home_page {
        let h = h.trim();
        // 지금 있는 페이지만 받는다. 나중에 지우거나 숨기면 첫 화면은 최신 글 목록으로 돌아간다.
        if !h.is_empty() && ctx.store.page_by_slug(h).await?.is_none() {
            return Err(crate::services::pages::not_found(h));
        }
        changes.push(("home_page", json!(h)));
    }
    if let Some(n) = patch.toc_min_headings {
        changes.push(("toc_min_headings", json!(n.min(20))));
    }
    if let Some(b) = patch.show_views {
        changes.push(("show_views", json!(b)));
    }
    if let Some(b) = patch.list_under_post {
        changes.push(("list_under_post", json!(b)));
    }
    if let Some(b) = patch.list_votes {
        changes.push(("list_votes", json!(b)));
    }
    if let Some(b) = patch.stickers {
        changes.push(("stickers", json!(b)));
    }
    if let Some(b) = patch.show_search {
        changes.push(("show_search", json!(b)));
    }
    if let Some(n) = patch.stats_retention_days {
        if !(30..=3650).contains(&n) {
            return Err(Error::new(Code::Validation)
                .with_message("error.stats_retention")
                .arg("value", n.to_string()));
        }
        changes.push(("stats_retention_days", json!(n)));
    }
    if let Some(raw) = &patch.banned_words {
        actor.require(Permission::MemberManage)?;
        changes.push(("banned_words", json!(moderation::parse_words(raw)?)));
    }
    if let Some(n) = patch.flood_seconds {
        actor.require(Permission::MemberManage)?;
        if n > moderation::MAX_FLOOD_SECONDS {
            return Err(Error::new(Code::Validation)
                .with_message("error.flood_seconds")
                .arg("value", n.to_string()));
        }
        changes.push(("flood_seconds", json!(n)));
    }
    if let Some(n) = patch.dormant_days {
        actor.require(Permission::MemberManage)?;
        if n != 0 && !(retention::MIN_DORMANT_DAYS..=retention::MAX_DAYS).contains(&n) {
            return Err(Error::new(Code::Validation)
                .with_message("error.dormant_days")
                .arg("value", n.to_string()));
        }
        changes.push(("dormant_days", json!(n)));
    }
    if let Some(n) = patch.mail_retention_days {
        actor.require(Permission::MemberManage)?;
        if !(retention::MIN_MAIL_DAYS..=retention::MAX_DAYS).contains(&n) {
            return Err(Error::new(Code::Validation)
                .with_message("error.mail_retention")
                .arg("value", n.to_string()));
        }
        changes.push(("mail_retention_days", json!(n)));
    }
    if let Some(n) = patch.signal_comment_burst {
        actor.require(Permission::MemberManage)?;
        if n != 0 && !(signals::MIN_BURST..=signals::MAX_BURST).contains(&n) {
            return Err(Error::new(Code::Validation)
                .with_message("error.signal_burst")
                .arg("value", n.to_string()));
        }
        changes.push(("signal_comment_burst", json!(n)));
    }
    if let Some(b) = patch.signal_mail {
        actor.require(Permission::MemberManage)?;
        changes.push(("signal_mail", json!(b)));
    }
    if let Some(b) = patch.link_cards {
        changes.push(("link_cards", json!(b)));
    }
    if let Some(u) = &patch.share_image {
        let u = u.trim();
        if !u.is_empty() && !share_image_ok(u) {
            return Err(Error::new(Code::Validation)
                .with_message("error.share_image")
                .arg("value", u.chars().take(80).collect::<String>()));
        }
        changes.push(("share_image", json!(u)));
    }
    if let Some(b) = patch.indexnow {
        changes.push(("indexnow", json!(b)));
    }
    if let Some(m) = &patch.signup {
        actor.require(Permission::MemberManage)?;
        let mode = SignupMode::parse(m.trim()).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.signup_option")
                .arg("value", m.chars().take(20).collect::<String>())
                .arg(
                    "allowed",
                    SignupMode::ALL.map(SignupMode::as_str).join(", "),
                )
        })?;
        changes.push(("signup", json!(mode)));
    }
    if let Some(b) = patch.signup_verify {
        actor.require(Permission::MemberManage)?;
        changes.push(("signup_verify", json!(b)));
    }
    if let Some(m) = &patch.captcha {
        actor.require(Permission::MemberManage)?;
        let mode = CaptchaMode::parse(m.trim()).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.captcha_option")
                .arg("value", m.chars().take(20).collect::<String>())
                .arg(
                    "allowed",
                    CaptchaMode::ALL.map(CaptchaMode::as_str).join(", "),
                )
        })?;
        changes.push(("captcha", json!(mode)));
    }
    if let Some(id) = &patch.skin {
        let id = id.trim();
        if !SKINS.iter().any(|(s, _)| *s == id) {
            return Err(Error::new(Code::Validation)
                .with_message("error.skin_unknown")
                .arg("value", id.chars().take(20).collect::<String>())
                .arg(
                    "allowed",
                    SKINS.iter().map(|(s, _)| *s).collect::<Vec<_>>().join(", "),
                ));
        }
        changes.push(("skin", json!(id)));
    }
    for (key, v) in [
        ("naver_verification", &patch.naver_verification),
        ("google_verification", &patch.google_verification),
    ] {
        if let Some(v) = v {
            let v = v.trim();
            let ok = v.len() <= 100
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !ok {
                return Err(Error::new(Code::Validation)
                    .with_message("error.setting_verification")
                    .arg("field", key)
                    .arg("value", v.chars().take(40).collect::<String>()));
            }
            changes.push((key, json!(v)));
        }
    }

    let mut tx = Tx::begin(ctx).await?;
    let before = from_pairs(tx.settings().await?);
    let mut after_value = serde_json::to_value(&before).map_err(Error::internal)?;
    for (k, v) in &changes {
        after_value[*k] = v.clone();
    }
    let after: SiteSettings = serde_json::from_value(after_value).map_err(Error::internal)?;
    if after == before {
        return Ok(before);
    }
    let now = clock::now();
    for (k, v) in &changes {
        tx.put_setting(&format!("site.{k}"), v, &now).await?;
    }
    tx.audit(
        actor,
        "settings.update",
        Target::site(),
        json!({ "before": before, "after": after }),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 사이트를 인터넷에 여는 일을 감사에 남긴다. 로컬 운영자만 한다. 에이전트 토큰은 admin 스코프여도 안 된다 (ADR-0118).
pub async fn record_expose(ctx: &Ctx, actor: &Actor, diff: Value) -> Result<()> {
    if !matches!(actor.kind(), crate::actor::ActorKind::Local) {
        return Err(Error::new(Code::Forbidden).with_message("error.expose_local"));
    }
    let mut tx = Tx::begin(ctx).await?;
    tx.audit(actor, "site.expose", Target::new("site", "tunnel"), diff)
        .await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_missing_and_bad_values() {
        let s = from_pairs(vec![
            ("site.name".into(), json!("내 블로그")),
            ("site.unknown".into(), json!(1)),
        ]);
        assert_eq!(s.name, "내 블로그");
        assert_eq!(s.lang, "ko");
        // 타입이 틀린 값이 있으면 전체 기본값으로 (화면이 깨지지 않게)
        let s = from_pairs(vec![("site.tz_offset_min".into(), json!("x"))]);
        assert_eq!(s, SiteSettings::default());
    }
}
