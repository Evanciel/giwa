//! 공식 모듈: 상담 버튼 (GAP S11, ADR-0080).
//!
//! 카카오톡 채널 1:1 채팅·전화·외부 상담 주소로 가는 단추 위젯 하나. 외부 스크립트를 싣지 않고
//! 링크만 둔다. 누르면 어디로 무엇이 가는지를 단추 아래에 늘 적는다.

use async_trait::async_trait;
use giwa_core::services::widgets::WidgetType;
use giwa_core::{Code, Ctx, Error, Result};
use giwa_modules::{Manifest, Module, module_error, schema_of};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

static MANIFEST: Manifest = Manifest {
    id: "consult",
    name: "상담 버튼",
    version: env!("CARGO_PKG_VERSION"),
    description: "카카오톡 채널 채팅·전화·상담 주소로 가는 단추를 위젯으로 놓습니다.",
    events: &[],
};

const WIDGET: &str = "consult_button";
const CHANNELS: [&str; 3] = ["kakao", "phone", "link"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsultWidget {
    /// `kakao`(카카오톡 채널 채팅) · `phone`(전화) · `link`(https 상담 주소)
    pub channel: String,
    /// kakao: 채널 홈 주소의 `_` 로 시작하는 ID · phone: 숫자와 `-` · link: `https://…`
    pub value: String,
    /// 단추 글자. 비우면 채널마다 기본값
    #[serde(default)]
    pub label: String,
    /// 단추 아래에 더 적을 안내 (상담 시간 등, 100자까지)
    #[serde(default)]
    pub note: String,
    /// 화면 오른쪽 아래에 떠 있게
    #[serde(default)]
    pub floating: bool,
}

#[derive(Clone, Default)]
pub struct ConsultModule;

impl ConsultModule {
    pub fn new() -> Self {
        Self
    }
}

fn bad(text: impl Into<String>) -> Error {
    module_error(Code::Validation, text)
}

/// 카카오톡 채널 공개 ID: `_` 다음 영문·숫자 (채널 관리자센터 > 상세설정의 홈 주소).
fn kakao_id_ok(v: &str) -> bool {
    v.len() <= 40
        && v.strip_prefix('_')
            .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn phone_ok(v: &str) -> bool {
    let digits = v.chars().filter(char::is_ascii_digit).count();
    (3..=15).contains(&digits)
        && v.chars().all(|c| c.is_ascii_digit() || c == '-')
        && !v.starts_with('-')
}

/// `https://호스트/…` 의 호스트. 공백·따옴표·꺾쇠가 있거나 호스트가 비면 `None`.
fn https_host(v: &str) -> Option<&str> {
    if v.len() > 300
        || v.chars()
            .any(|c| c.is_whitespace() || "\"'<>\\`".contains(c))
    {
        return None;
    }
    let rest = v.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    (!host.is_empty() && host.contains('.')).then_some(host)
}

fn check(mut c: ConsultWidget) -> Result<ConsultWidget> {
    c.channel = c.channel.trim().to_owned();
    c.value = c.value.trim().to_owned();
    c.label = c.label.trim().to_owned();
    c.note = c.note.trim().to_owned();
    if !CHANNELS.contains(&c.channel.as_str()) {
        return Err(bad(format!(
            "상담 버튼의 channel 은 {} 중 하나입니다.",
            CHANNELS.join(", ")
        )));
    }
    let ok = match c.channel.as_str() {
        "kakao" => kakao_id_ok(&c.value),
        "phone" => phone_ok(&c.value),
        _ => https_host(&c.value).is_some(),
    };
    if !ok {
        return Err(bad(match c.channel.as_str() {
            "kakao" => {
                "카카오톡 채널 ID 는 `_` 로 시작하는 영문·숫자입니다 (채널 관리자센터 > 상세설정의 홈 주소 끝부분)."
            }
            "phone" => "전화번호는 숫자와 `-` 로 적습니다. 예: 02-123-4567",
            _ => "상담 주소는 https:// 로 시작하는 주소입니다.",
        }));
    }
    if c.label.chars().count() > 20 {
        return Err(bad("상담 버튼 글자는 20자까지입니다."));
    }
    if c.note.chars().count() > 100 {
        return Err(bad("상담 버튼 안내는 100자까지입니다."));
    }
    Ok(c)
}

/// 템플릿에 넘길 값: 주소·글자·고지.
fn view(c: &ConsultWidget) -> Value {
    let (href, label, notice, short, external) = match c.channel.as_str() {
        "kakao" => (
            format!("https://pf.kakao.com/{}/chat", c.value),
            "카카오톡 상담",
            "누르면 카카오톡 채널 1:1 채팅으로 이동합니다. 대화는 카카오톡에서 이어지며 카카오의 개인정보 처리방침을 따릅니다.".to_owned(),
            "카카오톡으로 이동".to_owned(),
            true,
        ),
        "phone" => (
            format!("tel:{}", c.value),
            "전화 상담",
            format!("누르면 {} 로 전화를 겁니다.", c.value),
            format!("{} 전화 연결", c.value),
            false,
        ),
        _ => {
            let host = https_host(&c.value).unwrap_or_default();
            (
                c.value.clone(),
                "상담하기",
                format!("누르면 외부 사이트 {host} 로 이동합니다. 그 사이트의 개인정보 처리방침을 따릅니다."),
                format!("{host} 로 이동"),
                true,
            )
        }
    };
    json!({
        "channel": c.channel,
        "href": href,
        "label": if c.label.is_empty() { label } else { c.label.as_str() },
        "notice": notice,
        // 떠 있는 단추는 본문을 덜 가리게 갈 곳만 한 줄로 보이고, 전체 고지는 화면 낭독기용으로 둔다
        "short": short,
        "note": c.note,
        "floating": c.floating,
        "external": external,
    })
}

#[async_trait]
impl Module for ConsultModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![(
            "mod/consult/consult_button.html",
            include_str!("../templates/consult_button.html"),
        )]
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        vec![WidgetType {
            id: WIDGET,
            name: "상담 버튼",
            description: "카카오톡 채널 채팅·전화·상담 주소 단추와 외부 이동 안내",
            config_schema: schema_of::<ConsultWidget>(),
        }]
    }

    async fn check_widget(&self, _ctx: &Ctx, _kind: &str, config: Value) -> Result<Value> {
        let c: ConsultWidget =
            serde_json::from_value(config).map_err(|e| bad(format!("상담 버튼 위젯 설정: {e}")))?;
        serde_json::to_value(check(c)?).map_err(Error::internal)
    }

    async fn widget_view(&self, _ctx: &Ctx, _kind: &str, config: &Value) -> Result<Value> {
        let c: ConsultWidget = serde_json::from_value(config.clone()).map_err(Error::internal)?;
        Ok(view(&c))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn w(channel: &str, value: &str) -> ConsultWidget {
        ConsultWidget {
            channel: channel.into(),
            value: value.into(),
            label: String::new(),
            note: String::new(),
            floating: false,
        }
    }

    #[test]
    fn values_are_checked_per_channel() {
        assert!(check(w("kakao", "_abC12")).is_ok());
        for bad in ["abc", "_", "_a b", "_a/chat", "_<x>"] {
            assert!(check(w("kakao", bad)).is_err(), "{bad}");
        }
        assert!(check(w("phone", "02-123-4567")).is_ok());
        for bad in ["12", "02 123", "-0212345", "javascript:1"] {
            assert!(check(w("phone", bad)).is_err(), "{bad}");
        }
        assert!(check(w("link", "https://help.example.test/chat")).is_ok());
        for bad in [
            "http://help.example.test",
            "javascript:alert(1)",
            "https://",
            "https://a b.test",
            "https://x.test/\"onmouseover",
        ] {
            assert!(check(w("link", bad)).is_err(), "{bad}");
        }
        assert!(check(w("fax", "1")).is_err());
    }

    #[test]
    fn view_builds_href_and_notice() {
        let v = view(&check(w("kakao", " _abC12 ")).unwrap());
        assert_eq!(v["href"], "https://pf.kakao.com/_abC12/chat");
        assert_eq!(v["label"], "카카오톡 상담");
        assert!(v["notice"].as_str().unwrap().contains("카카오"));
        let v = view(&w("phone", "02-123-4567"));
        assert_eq!(v["href"], "tel:02-123-4567");
        let mut link = w("link", "https://user@help.example.test/chat?x=1");
        link.label = "문의".into();
        let v = view(&link);
        assert_eq!(v["label"], "문의");
        assert!(v["notice"].as_str().unwrap().contains("help.example.test"));
        assert_eq!(
            v["short"], "help.example.test 로 이동",
            "떠 있는 단추의 한 줄 고지"
        );
    }
}
