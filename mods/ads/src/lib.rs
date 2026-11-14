//! 공식 모듈: 광고 자리 (GAP S10, ADR-0079).
//!
//! 위젯 `ad_slot` 하나. 자체 배너(사이트 사진 + 링크)는 바깥으로 아무것도 보내지 않는다.
//! 구글 애드센스는 광고 코드가 쓰는 도메인이 바뀌어 주소 허용 목록을 지원하지 않으므로(공식 도움말),
//! 이 위젯이 있는 화면에만 nonce 를 쓰는 strict CSP 를 건다. 웹 계층이 위젯 값의 `csp: "strict"` 를 본다.

use async_trait::async_trait;
use giwa_core::services::widgets::WidgetType;
use giwa_core::{Code, Ctx, Error, Result};
use giwa_modules::{Manifest, Module, module_error, schema_of};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

static MANIFEST: Manifest = Manifest {
    id: "ads",
    name: "광고 자리",
    version: env!("CARGO_PKG_VERSION"),
    description: "위젯 영역에 자체 배너나 구글 애드센스 광고를 놓습니다. 광고에는 「광고」 표시가 붙습니다.",
    events: &[],
};

const WIDGET: &str = "ad_slot";
const PROVIDERS: [&str; 2] = ["house", "adsense"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdSlot {
    /// 광고 종류. `house`(자체 배너) · `adsense`(구글 애드센스)
    pub provider: String,
    /// 애드센스 게시자 ID. `ca-pub-` + 숫자
    #[serde(default)]
    pub client: String,
    /// 애드센스 광고 단위 ID (숫자)
    #[serde(default)]
    pub slot: String,
    /// house: 사이트에 올린 사진 주소 (`/media/…`)
    #[serde(default)]
    pub image: String,
    /// house: 누르면 갈 곳 (`/…` 또는 `https://…`)
    #[serde(default)]
    pub link: String,
    /// house: 사진 설명 (대체 글)
    #[serde(default)]
    pub alt: String,
}

#[derive(Clone, Default)]
pub struct AdsModule;

impl AdsModule {
    pub fn new() -> Self {
        Self
    }
}

fn bad(text: impl Into<String>) -> Error {
    module_error(Code::Validation, text)
}

fn safe_text(v: &str) -> bool {
    !v.chars()
        .any(|c| c.is_whitespace() || "\"'<>\\`".contains(c))
}

fn link_ok(v: &str) -> bool {
    v.len() <= 300
        && safe_text(v)
        && ((v.starts_with('/') && !v.starts_with("//"))
            || v.strip_prefix("https://").is_some_and(|r| {
                r.split(['/', '?', '#'])
                    .next()
                    .is_some_and(|h| h.contains('.'))
            }))
}

fn check(mut c: AdSlot) -> Result<AdSlot> {
    for v in [
        &mut c.provider,
        &mut c.client,
        &mut c.slot,
        &mut c.image,
        &mut c.link,
        &mut c.alt,
    ] {
        *v = v.trim().to_owned();
    }
    match c.provider.as_str() {
        "adsense" => {
            let client_ok = c.client.strip_prefix("ca-pub-").is_some_and(|d| {
                (10..=20).contains(&d.len()) && d.chars().all(|ch| ch.is_ascii_digit())
            });
            if !client_ok {
                return Err(bad(
                    "애드센스 client 는 `ca-pub-` 다음 숫자입니다 (애드센스 계정 > 설정 > 계정 정보의 게시자 ID).",
                ));
            }
            if !(5..=20).contains(&c.slot.len()) || !c.slot.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(bad("애드센스 slot 은 광고 단위의 숫자 ID 입니다."));
            }
            c.image.clear();
            c.link.clear();
            c.alt.clear();
        }
        "house" => {
            if !c.image.starts_with("/media/") || !safe_text(&c.image) || c.image.contains("..") {
                return Err(bad(
                    "자체 배너 image 는 사이트에 올린 사진 주소(/media/…)입니다.",
                ));
            }
            if !link_ok(&c.link) {
                return Err(bad("자체 배너 link 는 /… 또는 https://… 주소입니다."));
            }
            if c.alt.is_empty() || c.alt.chars().count() > 100 {
                return Err(bad("자체 배너 alt(사진 설명)는 1~100자입니다."));
            }
            c.client.clear();
            c.slot.clear();
        }
        _ => {
            return Err(bad(format!(
                "광고 자리의 provider 는 {} 중 하나입니다.",
                PROVIDERS.join(", ")
            )));
        }
    }
    Ok(c)
}

fn view(c: &AdSlot) -> Value {
    let external = c.provider == "adsense";
    json!({
        "provider": c.provider,
        "client": c.client,
        "slot": c.slot,
        "image": c.image,
        "link": c.link,
        "alt": c.alt,
        "external_link": c.link.starts_with("https://"),
        // 웹 계층이 이 화면에 nonce 를 쓰는 strict CSP 를 건다 (ADR-0079).
        "csp": if external { Value::from("strict") } else { Value::Null },
    })
}

#[async_trait]
impl Module for AdsModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![(
            "mod/ads/ad_slot.html",
            include_str!("../templates/ad_slot.html"),
        )]
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        vec![WidgetType {
            id: WIDGET,
            name: "광고 자리",
            description: "자체 배너 또는 구글 애드센스. 애드센스가 있는 화면은 보안 정책(CSP)이 넓어집니다",
            config_schema: schema_of::<AdSlot>(),
        }]
    }

    async fn check_widget(&self, _ctx: &Ctx, _kind: &str, config: Value) -> Result<Value> {
        let c: AdSlot =
            serde_json::from_value(config).map_err(|e| bad(format!("광고 자리 위젯 설정: {e}")))?;
        serde_json::to_value(check(c)?).map_err(Error::internal)
    }

    async fn widget_view(&self, _ctx: &Ctx, _kind: &str, config: &Value) -> Result<Value> {
        let c: AdSlot = serde_json::from_value(config.clone()).map_err(Error::internal)?;
        Ok(view(&c))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn adsense(client: &str, slot: &str) -> AdSlot {
        AdSlot {
            provider: "adsense".into(),
            client: client.into(),
            slot: slot.into(),
            image: "/media/x.jpg".into(),
            link: String::new(),
            alt: String::new(),
        }
    }

    fn house(image: &str, link: &str) -> AdSlot {
        AdSlot {
            provider: "house".into(),
            client: String::new(),
            slot: String::new(),
            image: image.into(),
            link: link.into(),
            alt: "봄 행사".into(),
        }
    }

    #[test]
    fn adsense_ids_are_checked_and_mark_the_page_strict() {
        let c = check(adsense("ca-pub-1234567890123456", "1234567890")).unwrap();
        assert!(c.image.is_empty(), "다른 공급자 칸은 지운다");
        assert_eq!(view(&c)["csp"], "strict");
        for (client, slot) in [
            ("pub-1234567890123456", "1234567890"),
            ("ca-pub-12a4567890", "1234567890"),
            ("ca-pub-1234567890123456", "12\"><script>"),
            ("ca-pub-1234567890123456", ""),
        ] {
            assert!(check(adsense(client, slot)).is_err(), "{client} {slot}");
        }
    }

    #[test]
    fn house_banner_stays_on_site() {
        let c = check(house(
            "/media/2026/11/abc.jpg",
            "https://shop.example.test/spring",
        ))
        .unwrap();
        let v = view(&c);
        assert!(v["csp"].is_null());
        assert_eq!(v["external_link"], true);
        assert!(check(house("/media/a.jpg", "/b/free")).is_ok());
        for (image, link) in [
            ("https://cdn.example.test/a.jpg", "/"),
            ("/media/../x", "/"),
            ("/media/a.jpg", "javascript:alert(1)"),
            ("/media/a.jpg", "//evil.test"),
            ("/media/a.jpg", "http://shop.example.test"),
        ] {
            assert!(check(house(image, link)).is_err(), "{image} {link}");
        }
        let mut no_alt = house("/media/a.jpg", "/");
        no_alt.alt.clear();
        assert!(check(no_alt).is_err());
        let mut other = house("/media/a.jpg", "/");
        other.provider = "adfit".into();
        assert!(check(other).is_err());
    }
}
