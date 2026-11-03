//! 여분 필드 (GAP C10, ADR-0057). ADR-0018 필드 타입의 첫 구현.
//!
//! 게시판이 `field_schema`(칸 정의 배열)를 갖고, 글은 `meta`(키 → 값 객체)를 갖는다.
//! 관리·공개·REST·CLI 가 모두 [`validate`] 하나로 값을 검사·정리한다. 폼은 문자열로 오므로 타입별로 바꾼다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use crate::error::{Code, Error, Result};

/// 게시판당 칸 수
pub const MAX_FIELDS: usize = 30;
const MAX_KEY: usize = 32;
const MAX_LABEL: usize = 40;
const MAX_HELP: usize = 200;
const MAX_OPTIONS: usize = 50;
const MAX_OPTION: usize = 100;
const MAX_TEXT: usize = 200;
const MAX_TEXTAREA: usize = 5000;
const MAX_URL: usize = 500;

/// 지금 받는 타입. ADR-0018 의 나머지(`markdown`·`media`·`ref`·`group`·`list`)는 다음 단계.
pub const SUPPORTED: [&str; 11] = [
    "text",
    "textarea",
    "number",
    "boolean",
    "date",
    "datetime",
    "select",
    "multiselect",
    "url",
    "email",
    "phone",
];

/// 칸 하나의 정의.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    /// 영문 소문자로 시작하고 영문 소문자·숫자·`_` 32자까지 (예: `price`)
    pub key: String,
    /// 화면에 보이는 이름 (예: 가격)
    pub label: String,
    /// text, textarea, number, boolean, date, datetime, select, multiselect, url, email, phone
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    /// select·multiselect 의 선택지
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<FieldOption>,
    /// 입력 칸 아래 도움말
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FieldOption {
    pub value: String,
    /// 비우면 value 를 그대로 보인다
    #[serde(default)]
    pub label: String,
}

fn invalid(key: &'static str) -> Error {
    Error::new(Code::Validation).with_message(key)
}

fn key_ok(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= MAX_KEY
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 게시판 칸 정의를 검사하고 앞뒤 공백을 다듬는다.
pub fn check_schema(defs: Vec<FieldDef>) -> Result<Vec<FieldDef>> {
    if defs.len() > MAX_FIELDS {
        return Err(invalid("error.field_count").arg("max", MAX_FIELDS.to_string()));
    }
    let mut out: Vec<FieldDef> = Vec::with_capacity(defs.len());
    for mut d in defs {
        d.key = d.key.trim().to_owned();
        if !key_ok(&d.key) {
            return Err(invalid("error.field_key").arg("value", d.key));
        }
        if out.iter().any(|o| o.key == d.key) {
            return Err(invalid("error.field_dup").arg("key", d.key));
        }
        d.kind = d.kind.trim().to_ascii_lowercase();
        if !SUPPORTED.contains(&d.kind.as_str()) {
            return Err(invalid("error.field_type")
                .arg("key", d.key)
                .arg("value", d.kind)
                .arg("allowed", SUPPORTED.join(", ")));
        }
        d.label = d.label.trim().to_owned();
        if d.label.is_empty() || d.label.chars().count() > MAX_LABEL {
            return Err(invalid("error.field_label")
                .arg("key", d.key)
                .arg("max", MAX_LABEL.to_string()));
        }
        d.help = d
            .help
            .map(|h| h.trim().chars().take(MAX_HELP).collect::<String>())
            .filter(|h| !h.is_empty());
        if matches!(d.kind.as_str(), "select" | "multiselect") {
            let mut seen: Vec<FieldOption> = Vec::new();
            for mut o in std::mem::take(&mut d.options) {
                o.value = o.value.trim().to_owned();
                o.label = o.label.trim().to_owned();
                if o.value.is_empty()
                    || o.value.chars().count() > MAX_OPTION
                    || o.label.chars().count() > MAX_OPTION
                    || seen.iter().any(|s| s.value == o.value)
                {
                    return Err(invalid("error.field_options").arg("key", d.key));
                }
                seen.push(o);
            }
            if seen.is_empty() || seen.len() > MAX_OPTIONS {
                return Err(invalid("error.field_options").arg("key", d.key));
            }
            d.options = seen;
        } else {
            d.options.clear();
        }
        out.push(d);
    }
    Ok(out)
}

/// 글의 값을 칸 정의로 검사·정리한다. 빈 값은 지운다.
/// `strict` 가 거짓이면(게시판을 옮기거나 복사할 때) 정의에 없는 키는 버리고 필수 칸은 묻지 않는다.
pub fn validate(
    schema: &[FieldDef],
    input: &Map<String, Value>,
    strict: bool,
) -> Result<Map<String, Value>> {
    if strict && let Some(k) = input.keys().find(|k| !schema.iter().any(|d| &d.key == *k)) {
        return Err(invalid("error.field_unknown").arg("key", k.clone()));
    }
    let mut out = Map::new();
    for d in schema {
        let raw = input.get(&d.key).unwrap_or(&Value::Null);
        match value_of(d, raw) {
            Ok(Some(v)) => {
                out.insert(d.key.clone(), v);
            }
            Ok(None) if strict && d.required => {
                return Err(invalid("error.field_required").arg("label", d.label.clone()));
            }
            Ok(None) => {}
            Err(e) if strict => return Err(e),
            Err(_) => {}
        }
    }
    Ok(out)
}

const DATE: &[BorrowedFormatItem<'static>] = format_description!("[year]-[month]-[day]");
const DATETIME: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]");

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn bad(d: &FieldDef, hint: &'static str) -> Error {
    invalid("error.field_value")
        .arg("label", d.label.clone())
        .arg("hint", hint)
}

fn value_of(d: &FieldDef, raw: &Value) -> Result<Option<Value>> {
    if raw.is_null() {
        return Ok(None);
    }
    if d.kind == "multiselect" {
        let picked: Vec<String> = match raw {
            Value::Array(a) => a.iter().filter_map(text_of).collect(),
            other => text_of(other)
                .map(|s| s.split(',').map(|p| p.trim().to_owned()).collect())
                .unwrap_or_default(),
        };
        let picked: Vec<&String> = picked.iter().filter(|p| !p.is_empty()).collect();
        if let Some(p) = picked
            .iter()
            .find(|p| !d.options.iter().any(|o| &&o.value == *p))
        {
            let _ = p;
            return Err(bad(d, "선택지에 없는 값"));
        }
        // 선택지 순서대로, 겹침 없이
        let list: Vec<Value> = d
            .options
            .iter()
            .filter(|o| picked.iter().any(|p| **p == o.value))
            .map(|o| Value::String(o.value.clone()))
            .collect();
        return Ok((!list.is_empty()).then_some(Value::Array(list)));
    }
    if d.kind == "boolean" {
        let on = match raw {
            Value::Bool(b) => *b,
            other => match text_of(other)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "" | "false" | "0" | "off" | "no" => false,
                "true" | "1" | "on" | "yes" => true,
                _ => return Err(bad(d, "예 또는 아니오")),
            },
        };
        // 꺼진 값은 저장하지 않는다(필수면 켜야 한다).
        return Ok(on.then_some(Value::Bool(true)));
    }
    let Some(s) = text_of(raw) else {
        return Err(bad(d, "글자 값"));
    };
    if s.is_empty() {
        return Ok(None);
    }
    let v = match d.kind.as_str() {
        "text" => {
            if s.contains(['\n', '\r']) || s.chars().count() > MAX_TEXT {
                return Err(bad(d, "한 줄, 200자까지"));
            }
            Value::String(s)
        }
        "textarea" => {
            if s.chars().count() > MAX_TEXTAREA {
                return Err(bad(d, "5000자까지"));
            }
            Value::String(s.replace("\r\n", "\n"))
        }
        "number" => {
            let plain: String = s
                .chars()
                .filter(|c| *c != ',' && !c.is_whitespace())
                .collect();
            if let Ok(i) = plain.parse::<i64>() {
                json!(i)
            } else {
                match plain.parse::<f64>() {
                    Ok(f) if f.is_finite() => json!(f),
                    _ => return Err(bad(d, "숫자")),
                }
            }
        }
        "date" => {
            time::Date::parse(&s, DATE).map_err(|_| bad(d, "2026-11-04 형식"))?;
            Value::String(s)
        }
        "datetime" => {
            let head = s.get(..16).unwrap_or(&s);
            time::PrimitiveDateTime::parse(head, DATETIME)
                .map_err(|_| bad(d, "2026-11-04T14:30 형식"))?;
            Value::String(head.to_owned())
        }
        "select" => {
            if !d.options.iter().any(|o| o.value == s) {
                return Err(bad(d, "선택지에 없는 값"));
            }
            Value::String(s)
        }
        "url" => {
            let lower = s.to_ascii_lowercase();
            if !(lower.starts_with("https://") || lower.starts_with("http://"))
                || s.len() > MAX_URL
                || s.chars().any(char::is_whitespace)
                || s.len() <= "https://".len()
            {
                return Err(bad(d, "http:// 또는 https:// 로 시작하는 주소"));
            }
            Value::String(s)
        }
        "email" => {
            let ok = s.len() <= MAX_TEXT
                && !s.chars().any(char::is_whitespace)
                && s.split_once('@').is_some_and(|(l, h)| {
                    !l.is_empty()
                        && h.contains('.')
                        && !h.starts_with('.')
                        && !h.ends_with('.')
                        && !h.contains('@')
                });
            if !ok {
                return Err(bad(d, "메일 주소"));
            }
            Value::String(s)
        }
        "phone" => {
            let digits = s.chars().filter(char::is_ascii_digit).count();
            let ok = s.chars().count() <= 30
                && digits >= 7
                && s.chars().all(|c| c.is_ascii_digit() || "+-() ".contains(c));
            if !ok {
                return Err(bad(d, "전화번호 (숫자·+·-)"));
            }
            Value::String(s)
        }
        _ => return Err(bad(d, "지원하지 않는 형식")),
    };
    Ok(Some(v))
}

/// 공개 화면에 보일 값 목록. 지금 정의에 있는 키만, 정의 순서대로.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Shown {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
    /// url·email·phone 은 누를 수 있는 주소
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

pub fn display(schema: &[FieldDef], meta: &Map<String, Value>) -> Vec<Shown> {
    let mut out = Vec::new();
    for d in schema {
        let Some(v) = meta.get(&d.key) else { continue };
        let label_of = |value: &str| {
            d.options
                .iter()
                .find(|o| o.value == value)
                .map(|o| {
                    if o.label.is_empty() {
                        o.value.clone()
                    } else {
                        o.label.clone()
                    }
                })
                .unwrap_or_else(|| value.to_owned())
        };
        let (text, href) = match (d.kind.as_str(), v) {
            ("boolean", Value::Bool(true)) => ("예".to_owned(), None),
            ("number", Value::Number(n)) => (group_digits(&n.to_string()), None),
            ("select", Value::String(s)) => (label_of(s), None),
            ("multiselect", Value::Array(a)) => (
                a.iter()
                    .filter_map(Value::as_str)
                    .map(label_of)
                    .collect::<Vec<_>>()
                    .join(", "),
                None,
            ),
            ("date", Value::String(s)) => (dotted_date(s), None),
            ("datetime", Value::String(s)) => {
                let (day, at) = s.split_once('T').unwrap_or((s, ""));
                (format!("{} {at}", dotted_date(day)).trim().to_owned(), None)
            }
            ("url", Value::String(s)) => (s.clone(), Some(s.clone())),
            ("email", Value::String(s)) => (s.clone(), Some(format!("mailto:{s}"))),
            ("phone", Value::String(s)) => (
                s.clone(),
                Some(format!(
                    "tel:{}",
                    s.chars()
                        .filter(|c| c.is_ascii_digit() || *c == '+')
                        .collect::<String>()
                )),
            ),
            (_, Value::String(s)) => (s.clone(), None),
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        out.push(Shown {
            key: d.key.clone(),
            label: d.label.clone(),
            kind: d.kind.clone(),
            text,
            href,
        });
    }
    out
}

/// `2026-11-04` → `2026. 11. 4.`
fn dotted_date(s: &str) -> String {
    match time::Date::parse(s, DATE) {
        Ok(d) => format!("{}. {}. {}.", d.year(), u8::from(d.month()), d.day()),
        Err(_) => s.to_owned(),
    }
}

/// 정수 부분에 천 단위 쉼표.
fn group_digits(n: &str) -> String {
    let (sign, rest) = n.strip_prefix('-').map_or(("", n), |r| ("-", r));
    let (int, frac) = rest
        .split_once('.')
        .map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn def(key: &str, kind: &str, required: bool) -> FieldDef {
        FieldDef {
            key: key.into(),
            label: key.into(),
            kind: kind.into(),
            required,
            options: Vec::new(),
            help: None,
        }
    }

    fn opts(mut d: FieldDef, values: &[&str]) -> FieldDef {
        d.options = values
            .iter()
            .map(|v| FieldOption {
                value: (*v).into(),
                label: format!("{v}!"),
            })
            .collect();
        d
    }

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn schema_rules() {
        assert!(check_schema(vec![def("price", "number", true)]).is_ok());
        for bad in ["Price", "1st", "a-b", "", &"a".repeat(33)] {
            assert_eq!(
                check_schema(vec![def(bad, "text", false)])
                    .unwrap_err()
                    .code(),
                Code::Validation
            );
        }
        let dup = check_schema(vec![def("a", "text", false), def("a", "text", false)]).unwrap_err();
        assert_eq!(dup.message(), "같은 키(a)의 칸이 두 번 있습니다.");
        assert!(check_schema(vec![def("photo", "media", false)]).is_err());
        assert!(
            check_schema(vec![def("area", "select", false)]).is_err(),
            "선택지 없음"
        );
        let ok = check_schema(vec![opts(def("area", "select", false), &["seoul"])]).unwrap();
        assert_eq!(ok[0].options.len(), 1);
        let cleared = check_schema(vec![opts(def("t", "text", false), &["x"])]).unwrap();
        assert!(cleared[0].options.is_empty());
        let many = (0..31)
            .map(|i| def(&format!("f{i}"), "text", false))
            .collect();
        assert!(check_schema(many).is_err());
    }

    #[test]
    fn values_are_converted_by_type() {
        let schema = vec![
            def("price", "number", true),
            def("ok", "boolean", false),
            def("day", "date", false),
            def("at", "datetime", false),
            opts(def("area", "select", false), &["seoul", "busan"]),
            opts(def("tags", "multiselect", false), &["a", "b", "c"]),
            def("site", "url", false),
            def("mail", "email", false),
            def("tel", "phone", false),
            def("memo", "text", false),
        ];
        let out = validate(
            &schema,
            &map(json!({
                "price": "12,000", "ok": "on", "day": "2026-11-04", "at": "2026-11-04T14:30:00",
                "area": "busan", "tags": ["c", "a", "c"], "site": "https://example.test/a",
                "mail": "me@example.test", "tel": "010-1234-5678", "memo": "  ",
            })),
            true,
        )
        .unwrap();
        assert_eq!(out["price"], json!(12000));
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["at"], json!("2026-11-04T14:30"));
        assert_eq!(out["tags"], json!(["a", "c"]));
        assert!(!out.contains_key("memo"), "빈 값은 지운다");
        let shown = display(&schema, &out);
        let text = |k: &str| shown.iter().find(|s| s.key == k).unwrap().text.clone();
        assert_eq!(text("price"), "12,000");
        assert_eq!(text("area"), "busan!");
        assert_eq!(text("tags"), "a!, c!");
        assert_eq!(text("day"), "2026. 11. 4.");
        assert_eq!(
            shown
                .iter()
                .find(|s| s.key == "tel")
                .unwrap()
                .href
                .as_deref(),
            Some("tel:01012345678")
        );
    }

    #[test]
    fn strict_and_lenient() {
        let schema = vec![
            def("price", "number", true),
            opts(def("area", "select", false), &["seoul"]),
        ];
        let e = validate(&schema, &map(json!({"area": "seoul"})), true).unwrap_err();
        assert_eq!(e.message(), "price 칸을 채워 주세요.");
        let e = validate(&schema, &map(json!({"price": 1, "x": 1})), true).unwrap_err();
        assert_eq!(e.code(), Code::Validation);
        let e = validate(&schema, &map(json!({"price": "열"})), true).unwrap_err();
        assert_eq!(e.message(), "price 칸 값이 올바르지 않습니다 (숫자).");
        assert!(validate(&schema, &map(json!({"price": 1, "area": "jeju"})), true).is_err());
        for bad in ["ftp://x.test", "https://", "me@", "12"] {
            let s = vec![
                def("u", "url", false),
                def("m", "email", false),
                def("p", "phone", false),
            ];
            let key = if bad.starts_with("ftp") || bad.starts_with("https") {
                "u"
            } else if bad.contains('@') {
                "m"
            } else {
                "p"
            };
            assert!(
                validate(&s, &map(json!({ key: bad })), true).is_err(),
                "{bad}"
            );
        }
        // 옮길 때: 모르는 키·잘못된 값은 버리고 필수는 묻지 않는다.
        let out = validate(&schema, &map(json!({"x": 1, "area": "jeju"})), false).unwrap();
        assert!(out.is_empty());
    }
}
