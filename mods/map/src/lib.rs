//! 공식 모듈: 지도 (SPEC §13, ADR-0130).
//!
//! 위젯 `map` 하나. 키가 없으면 주소와 지도 링크만 보여 바깥으로 아무것도 보내지 않는다.
//! 키를 넣으면 그 화면에서만 방문자의 브라우저가 카카오·네이버 지도 코드를 받는다.
//! 지도 코드는 안에서 스크립트를 더 부르므로 광고와 같이 nonce 를 쓰는 strict CSP 를 건다.

use async_trait::async_trait;
use maru_core::directives::map_search_urls;
use maru_core::services::widgets::WidgetType;
use maru_core::{Code, Ctx, Error, Result};
use maru_modules::{Manifest, Module, module_error, schema_of};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

static MANIFEST: Manifest = Manifest {
    id: "map",
    name: "지도",
    version: env!("CARGO_PKG_VERSION"),
    description: "주소를 지도로 보여 줍니다. 키가 없으면 네이버 지도·카카오맵으로 가는 링크만 둡니다.",
    events: &[],
};

const WIDGET: &str = "map";
const PROVIDERS: [&str; 3] = ["link", "kakao", "naver"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MapWidget {
    /// 찾을 주소 (도로명 권장, 200자까지)
    pub address: String,
    /// 장소 이름. 주소 위에 굵게 적힌다
    #[serde(default)]
    pub name: String,
    /// `link`(지도 링크만, 기본값) · `kakao`(카카오맵) · `naver`(네이버 지도)
    #[serde(default)]
    pub provider: String,
    /// kakao: JavaScript 키 · naver: Client ID. 등록한 사이트 주소에서만 동작한다
    #[serde(default)]
    pub key: String,
    /// 위도. 경도와 함께 넣으면 주소 변환을 건너뛴다
    #[serde(default)]
    pub lat: Option<f64>,
    /// 경도
    #[serde(default)]
    pub lng: Option<f64>,
}

#[derive(Clone, Default)]
pub struct MapModule;

impl MapModule {
    pub fn new() -> Self {
        Self
    }
}

fn bad(text: impl Into<String>) -> Error {
    module_error(Code::Validation, text)
}

fn check(mut c: MapWidget) -> Result<MapWidget> {
    for v in [&mut c.address, &mut c.name, &mut c.provider, &mut c.key] {
        *v = v.trim().to_owned();
    }
    if c.provider.is_empty() {
        c.provider = "link".to_owned();
    }
    if !PROVIDERS.contains(&c.provider.as_str()) {
        return Err(bad(format!(
            "지도의 provider 는 {} 중 하나입니다.",
            PROVIDERS.join(", ")
        )));
    }
    let len = c.address.chars().count();
    if len == 0 || len > 200 {
        return Err(bad("지도의 address 는 1~200자입니다."));
    }
    if c.name.chars().count() > 100 {
        return Err(bad("지도의 name 은 100자까지입니다."));
    }
    if c.provider == "link" {
        c.key.clear();
    } else if !(6..=64).contains(&c.key.len())
        || !c.key.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        return Err(bad(
            "지도의 key 는 영문과 숫자입니다. 카카오는 JavaScript 키, 네이버는 Client ID 를 넣습니다.",
        ));
    }
    match (c.lat, c.lng) {
        (None, None) => {}
        (Some(lat), Some(lng))
            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng) => {}
        _ => {
            return Err(bad(
                "지도의 lat·lng 은 함께 넣습니다. 위도는 -90~90, 경도는 -180~180 입니다.",
            ));
        }
    }
    Ok(c)
}

fn view(c: &MapWidget) -> Value {
    let (naver, kakao) = map_search_urls(&c.address);
    let live = c.provider != "link";
    json!({
        "provider": c.provider,
        "key": c.key,
        "address": c.address,
        "name": c.name,
        "lat": c.lat,
        "lng": c.lng,
        "naver_url": naver,
        "kakao_url": kakao,
        "live": live,
        // 웹 계층이 이 화면에 nonce 를 쓰는 strict CSP 를 건다 (ADR-0079).
        "csp": if live { Value::from("strict") } else { Value::Null },
    })
}

#[async_trait]
impl Module for MapModule {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        vec![("mod/map/map.html", include_str!("../templates/map.html"))]
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        vec![WidgetType {
            id: WIDGET,
            name: "지도",
            description: "주소와 지도 링크. 카카오·네이버 키를 넣으면 지도가 그 자리에 뜨고, 그 화면의 보안 정책(CSP)이 넓어집니다",
            config_schema: schema_of::<MapWidget>(),
        }]
    }

    async fn check_widget(&self, _ctx: &Ctx, _kind: &str, config: Value) -> Result<Value> {
        let c: MapWidget =
            serde_json::from_value(config).map_err(|e| bad(format!("지도 위젯 설정: {e}")))?;
        serde_json::to_value(check(c)?).map_err(Error::internal)
    }

    async fn widget_view(&self, _ctx: &Ctx, _kind: &str, config: &Value) -> Result<Value> {
        let c: MapWidget = serde_json::from_value(config.clone()).map_err(Error::internal)?;
        Ok(view(&c))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn w(provider: &str, key: &str) -> MapWidget {
        MapWidget {
            address: "서울 중구 세종대로 110".into(),
            name: String::new(),
            provider: provider.into(),
            key: key.into(),
            lat: None,
            lng: None,
        }
    }

    #[test]
    fn 키가_없으면_바깥으로_아무것도_보내지_않는다() {
        let c = check(w("", "남은값")).unwrap();
        assert_eq!(c.provider, "link");
        assert!(c.key.is_empty());
        let v = view(&c);
        assert!(v["csp"].is_null());
        assert_eq!(v["live"], false);
        assert!(
            v["naver_url"]
                .as_str()
                .unwrap()
                .starts_with("https://map.naver.com/")
        );
        assert!(
            v["kakao_url"]
                .as_str()
                .unwrap()
                .starts_with("https://map.kakao.com/")
        );
    }

    #[test]
    fn 키는_영문과_숫자만_받고_지도가_뜨는_화면만_넓힌다() {
        let c = check(w("kakao", "abcDEF0123456789")).unwrap();
        assert_eq!(view(&c)["csp"], "strict");
        assert!(check(w("naver", "abc123")).is_ok());
        for key in ["", "abc", "abc\"><script>", "abc 123", "키값입니다아아아"] {
            assert!(check(w("kakao", key)).is_err(), "{key}");
        }
        assert!(check(w("google", "abcDEF0123")).is_err());
    }

    #[test]
    fn 좌표는_함께_넣고_범위를_지킨다() {
        let with = |lat, lng| {
            let mut c = w("link", "");
            c.lat = lat;
            c.lng = lng;
            check(c)
        };
        assert!(with(Some(37.5), Some(127.0)).is_ok());
        assert!(with(Some(37.5), None).is_err());
        assert!(with(None, Some(127.0)).is_err());
        assert!(with(Some(91.0), Some(127.0)).is_err());
        assert!(with(Some(37.5), Some(181.0)).is_err());
        let mut empty = w("link", "");
        empty.address = "  ".into();
        assert!(check(empty).is_err());
    }
}
