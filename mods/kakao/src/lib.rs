//! 공식 모듈: 카카오톡 공유와 채널 (SPEC §13, ADR-0130).
//!
//! `kakao_share` 는 지금 화면을 카카오톡으로 보내는 단추다. 카카오 공유는 SDK 로만 되므로 그 화면에서만
//! SDK 를 받고 nonce 를 쓰는 strict CSP 를 건다. SDK 판과 무결성 값은 공식 다운로드 페이지에서 옮겼다.
//! `kakao_channel` 은 채널 홈과 채팅으로 가는 링크뿐이라 키도 스크립트도 없다.

use async_trait::async_trait;
use maru_core::services::widgets::WidgetType;
use maru_core::{Code, Ctx, Error, Result};
use maru_modules::{Manifest, Module, module_error, schema_of};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

static MANIFEST: Manifest = Manifest {
    id: "kakao",
    name: "카카오톡 공유와 채널",
    version: env!("CARGO_PKG_VERSION"),
    description: "지금 화면을 카카오톡으로 공유하는 단추와 카카오톡 채널 단추를 위젯으로 놓습니다.",
    events: &[],
};

const SHARE: &str = "kakao_share";
const CHANNEL: &str = "kakao_channel";
const SDK_URL: &str = "https://t1.kakaocdn.net/kakao_js_sdk/2.8.4/kakao.min.js";
const SDK_INTEGRITY: &str =
    "sha384-BGX7Wb16GmRFagri4bLJV2DgipVZaU2AI812GZtHtHzzDQJnfHasDXN5houxv45w";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShareWidget {
    /// 카카오 앱의 JavaScript 키. 다른 키(REST API, Admin)를 넣지 않는다
    pub key: String,
    /// 단추 글자. 비우면 「카카오톡 공유」
    #[serde(default)]
    pub label: String,
    /// 공유 카드 제목. 비우면 화면 제목
    #[serde(default)]
    pub title: String,
    /// 공유 카드 설명. 비우면 화면 설명
    #[serde(default)]
    pub description: String,
    /// 공유 카드 사진 (`/media/…` 또는 `https://…`). 비우면 화면 대표 사진
    #[serde(default)]
    pub image: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChannelWidget {
    /// 채널 홈 주소의 `_` 로 시작하는 ID
    pub channel: String,
    /// 채널 이름. 단추 위에 적힌다
    #[serde(default)]
    pub name: String,
    /// 1:1 채팅 단추도 둔다
    #[serde(default)]
    pub chat: bool,
}

#[derive(Clone, Default)]
pub struct KakaoModule;

impl KakaoModule {
    pub fn new() -> Self {
        Self
    }
}

fn bad(text: impl Into<String>) -> Error {
    module_error(Code::Validation, text)
}

fn image_ok(v: &str) -> bool {
    let plain = !v
        .chars()
        .any(|c| c.is_whitespace() || "\"'<>\\`".contains(c));
    v.len() <= 500
        && plain
        && !v.contains("..")
        && (v.starts_with("/media/") || v.starts_with("https://"))
}

fn check_share(mut c: ShareWidget) -> Result<ShareWidget> {
    for v in [
        &mut c.key,
        &mut c.label,
        &mut c.title,
        &mut c.description,
        &mut c.image,
    ] {
        *v = v.trim().to_owned();
    }
    if !(16..=64).contains(&c.key.len()) || !c.key.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(bad(
            "카카오톡 공유의 key 는 JavaScript 키입니다 (카카오 개발자 콘솔의 앱 키).",
        ));
    }
    if c.label.chars().count() > 30 {
        return Err(bad("카카오톡 공유의 label 은 30자까지입니다."));
    }
    if c.title.chars().count() > 100 {
        return Err(bad("카카오톡 공유의 title 은 100자까지입니다."));
    }
    if c.description.chars().count() > 200 {
        return Err(bad("카카오톡 공유의 description 은 200자까지입니다."));
    }
    if !c.image.is_empty() && !image_ok(&c.image) {
        return Err(bad(
            "카카오톡 공유의 image 는 /media/… 또는 https://… 주소입니다.",
        ));
    }
    Ok(c)
}

fn check_channel(mut c: ChannelWidget) -> Result<ChannelWidget> {
    c.channel = c.channel.trim().to_owned();
    c.name = c.name.trim().to_owned();
    let id_ok = c.channel.len() <= 40
        && c.channel
            .strip_prefix('_')
            .is_some_and(|r| !r.is_empty() && r.chars().all(|ch| ch.is_ascii_alphanumeric()));
    if !id_ok {
        return Err(bad(
            "카카오톡 채널의 channel 은 채널 홈 주소의 `_` 로 시작하는 ID 입니다.",
        ));
    }
    if c.name.chars().count() > 50 {
        return Err(bad("카카오톡 채널의 name 은 50자까지입니다."));
    }
    Ok(c)
}

fn share_view(c: &ShareWidget) -> Value {
    json!({
        "key": c.key,
        "label": if c.label.is_empty() { "카카오톡 공유" } else { c.label.as_str() },
        "title": c.title,
        "description": c.description,
        "image": c.image,
        "sdk_url": SDK_URL,
        "sdk_integrity": SDK_INTEGRITY,
        // 웹 계층이 이 화면에 nonce 를 쓰는 strict CSP 를 건다 (ADR-0079).
        "csp": "strict",
    })
}

fn channel_view(c: &ChannelWidget) -> Value {
    json!({
        "name": c.name,
        "home_url": format!("https://pf.kakao.com/{}", c.channel),
        "chat_url": if c.chat { Value::from(format!("https://pf.kakao.com/{}/chat", c.channel)) } else { Value::Null },
    })
}

#[async_trait]
impl Module for KakaoModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "mod/kakao/kakao_share.html",
                include_str!("../templates/kakao_share.html"),
            ),
            (
                "mod/kakao/kakao_channel.html",
                include_str!("../templates/kakao_channel.html"),
            ),
        ]
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        vec![
            WidgetType {
                id: SHARE,
                name: "카카오톡 공유",
                description: "지금 화면을 카카오톡으로 보내는 단추. JavaScript 키가 필요하고, 그 화면의 보안 정책(CSP)이 넓어집니다",
                config_schema: schema_of::<ShareWidget>(),
            },
            WidgetType {
                id: CHANNEL,
                name: "카카오톡 채널",
                description: "채널 홈과 1:1 채팅으로 가는 단추. 키가 필요 없습니다",
                config_schema: schema_of::<ChannelWidget>(),
            },
        ]
    }

    async fn check_widget(&self, _ctx: &Ctx, kind: &str, config: Value) -> Result<Value> {
        if kind == SHARE {
            let c: ShareWidget = serde_json::from_value(config)
                .map_err(|e| bad(format!("카카오톡 공유 위젯 설정: {e}")))?;
            serde_json::to_value(check_share(c)?).map_err(Error::internal)
        } else {
            let c: ChannelWidget = serde_json::from_value(config)
                .map_err(|e| bad(format!("카카오톡 채널 위젯 설정: {e}")))?;
            serde_json::to_value(check_channel(c)?).map_err(Error::internal)
        }
    }

    async fn widget_view(&self, _ctx: &Ctx, kind: &str, config: &Value) -> Result<Value> {
        if kind == SHARE {
            let c: ShareWidget = serde_json::from_value(config.clone()).map_err(Error::internal)?;
            Ok(share_view(&c))
        } else {
            let c: ChannelWidget =
                serde_json::from_value(config.clone()).map_err(Error::internal)?;
            Ok(channel_view(&c))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn share(key: &str, image: &str) -> ShareWidget {
        ShareWidget {
            key: key.into(),
            label: String::new(),
            title: String::new(),
            description: String::new(),
            image: image.into(),
        }
    }

    #[test]
    fn 공유는_키를_보고_그_화면만_넓힌다() {
        let key = "abcdef0123456789abcdef0123456789";
        let v = share_view(&check_share(share(key, "/media/2026/11/a.jpg")).unwrap());
        assert_eq!(v["csp"], "strict");
        assert_eq!(v["label"], "카카오톡 공유");
        assert!(v["sdk_integrity"].as_str().unwrap().starts_with("sha384-"));
        assert!(check_share(share(key, "https://cdn.example.test/a.jpg")).is_ok());
        for bad_key in [
            "",
            "short",
            "abc\"><script>alert(1)</script>",
            "abc def0123456789",
        ] {
            assert!(check_share(share(bad_key, "")).is_err(), "{bad_key}");
        }
        for image in [
            "javascript:alert(1)",
            "/media/../x",
            "//evil.test/a.jpg",
            "/assets/a\".jpg",
        ] {
            assert!(check_share(share(key, image)).is_err(), "{image}");
        }
    }

    #[test]
    fn 채널은_링크뿐이다() {
        let c = ChannelWidget {
            channel: "_abC12".into(),
            name: String::new(),
            chat: true,
        };
        let v = channel_view(&check_channel(c.clone()).unwrap());
        assert_eq!(v["home_url"], "https://pf.kakao.com/_abC12");
        assert_eq!(v["chat_url"], "https://pf.kakao.com/_abC12/chat");
        assert!(v.get("csp").is_none());
        let mut no_chat = c.clone();
        no_chat.chat = false;
        assert!(channel_view(&no_chat)["chat_url"].is_null());
        for id in ["abC12", "_", "_a/b", "_a\"b"] {
            let mut bad = c.clone();
            bad.channel = id.into();
            assert!(check_channel(bad).is_err(), "{id}");
        }
    }
}
