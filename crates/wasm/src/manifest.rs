//! `module.toml` 읽기와 검사 (Phase 5 계획 「패키지」, ADR-0177).

use std::path::Path;

use giwa_core::actor::Permission;
use giwa_core::fields::FieldDef;
use giwa_core::{Code, Error, Result};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// 이 바이너리가 아는 호스트 API. 손님의 `module.toml` 의 `api` 가 이것이어야 한다.
pub const API: &str = "giwa:module@0.1";

/// 화면 자리 이름 (giwa-modules 의 상수와 같다). 외부 모듈은 여기서 고른 자리만 받는다.
const SLOTS: [&str; 7] = [
    giwa_modules::POST_FOOTER,
    giwa_modules::COMMENT_ACTIONS,
    giwa_modules::ME_SECTIONS,
    giwa_modules::ACCOUNT_BAR,
    giwa_modules::POST_LIST,
    giwa_modules::MEMBER_BADGE,
    giwa_modules::LOGIN_OPTIONS,
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleToml {
    pub module: Info,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub settings: Vec<FieldDef>,
    #[serde(default)]
    pub defaults: Map<String, Value>,
    #[serde(default)]
    pub widgets: Vec<Widget>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub api: String,
    /// 받을 감사 동작 이름
    #[serde(default)]
    pub events: Vec<String>,
    /// 채울 화면 자리. 적지 않은 자리는 부르지 않는다(공개 화면마다 인스턴스를 만들지 않게)
    #[serde(default)]
    pub slots: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 코어 권한 이름 (`post.read` 등)
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    /// `<모듈 id>_` 로 시작한다 (내장 위젯과 겹치지 않게)
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub config_schema: Option<Value>,
}

pub fn bad(path: &Path, detail: impl Into<String>) -> Error {
    Error::new(Code::Validation)
        .with_message("error.module_message")
        .arg(
            "text",
            format!("외부 모듈 {}: {}", path.display(), detail.into()),
        )
}

fn ident(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl ModuleToml {
    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let m: Self = toml::from_str(text).map_err(|e| bad(path, e.to_string()))?;
        m.check(path)?;
        Ok(m)
    }

    fn check(&self, path: &Path) -> Result<()> {
        let i = &self.module;
        if !ident(&i.id, 32) {
            return Err(bad(
                path,
                "id 는 영문 소문자로 시작하고 영문 소문자·숫자·_ 32자까지입니다",
            ));
        }
        if i.api != API {
            return Err(bad(
                path,
                format!("api 가 {} 입니다. 이 기와는 {API} 만 읽습니다", i.api),
            ));
        }
        if i.name.trim().is_empty() || i.name.chars().count() > 50 {
            return Err(bad(path, "name 은 1~50자입니다"));
        }
        if i.version.is_empty() || i.version.len() > 32 {
            return Err(bad(path, "version 은 1~32자입니다"));
        }
        if let Some(s) = i.slots.iter().find(|s| !SLOTS.contains(&s.as_str())) {
            return Err(bad(path, format!("모르는 화면 자리입니다: {s}")));
        }
        for (n, t) in self.tools.iter().enumerate() {
            if !ident(&t.name, 64) || self.tools[..n].iter().any(|o| o.name == t.name) {
                return Err(bad(
                    path,
                    format!("툴 이름이 잘못됐거나 겹칩니다: {}", t.name),
                ));
            }
            if let Some(p) = &t.permission
                && Permission::parse(p).is_none()
            {
                return Err(bad(path, format!("모르는 권한입니다: {p}")));
            }
        }
        let prefix = format!("{}_", i.id);
        for (n, w) in self.widgets.iter().enumerate() {
            if !w.id.starts_with(&prefix)
                || !ident(&w.id, 64)
                || self.widgets[..n].iter().any(|o| o.id == w.id)
            {
                return Err(bad(
                    path,
                    format!(
                        "위젯 id 는 {prefix} 로 시작하고 겹치지 않아야 합니다: {}",
                        w.id
                    ),
                ));
            }
        }
        Ok(())
    }

    /// 툴 입력 스키마. 적지 않았으면 아무 칸이나 받는 객체.
    pub fn schema(v: &Option<Value>) -> Value {
        v.clone()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }))
    }
}

/// 테마로 그릴 템플릿에 날 HTML 을 내는 문법이 있는가. 모듈이 준 값은 언제나 이스케이프돼야 한다.
pub fn unsafe_template(text: &str) -> bool {
    let squeezed: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    squeezed.contains("|safe") || squeezed.contains("autoescapefalse")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const GOOD: &str = r#"
[module]
id = "echo"
name = "메아리"
version = "0.1.0"
api = "giwa:module@0.1"
events = ["post.publish"]
slots = ["post_footer"]

[[tools]]
name = "count"
permission = "post.read"

[[widgets]]
id = "echo_box"
name = "메아리 상자"
"#;

    #[test]
    fn reads_good_manifest() {
        let m = ModuleToml::parse(Path::new("m"), GOOD).unwrap();
        assert_eq!(m.module.id, "echo");
        assert_eq!(m.tools[0].permission.as_deref(), Some("post.read"));
    }

    #[test]
    fn rejects_bad_fields() {
        for (from, to) in [
            ("id = \"echo\"", "id = \"Echo\""),
            ("giwa:module@0.1", "giwa:module@9.9"),
            ("\"post.read\"", "\"post.everything\""),
            ("\"post_footer\"", "\"nowhere\""),
            ("\"echo_box\"", "\"box\""),
            ("[[tools]]", "unknown = 1\n[[tools]]"),
        ] {
            let text = GOOD.replace(from, to);
            assert!(ModuleToml::parse(Path::new("m"), &text).is_err(), "{to}");
        }
    }

    #[test]
    fn finds_raw_html_in_templates() {
        assert!(unsafe_template("{{ x | safe }}"));
        assert!(unsafe_template(
            "{% autoescape false %}{{ x }}{% endautoescape %}"
        ));
        assert!(!unsafe_template("{{ x }} safe 한 글"));
    }
}
