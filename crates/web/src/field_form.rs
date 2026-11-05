//! 여분 칸 입력 (ADR-0057). 공개 글쓰기와 관리 편집기가 같이 쓴다.
//!
//! 폼 이름은 `<앞머리><키>`, 여러 개 고르기는 `<앞머리><키>.<선택지 값>`. 칸 묶음을 그렸다는 표시로
//! `<앞머리>_shown` 을 함께 보낸다(키는 영문 소문자로 시작하므로 겹치지 않는다). 표시가 없으면 값을 건드리지 않는다.

use std::collections::HashMap;

use maru_core::fields::FieldDef;
use serde_json::{Map, Value, json};

/// 공개 글쓰기는 게시판이 하나라 `field.`, 관리 편집기는 게시판을 바꿀 수 있어 `field.<게시판>.`.
pub fn prefix(board: Option<&str>) -> String {
    match board {
        Some(b) => format!("field.{b}."),
        None => "field.".to_owned(),
    }
}

pub fn shown_name(prefix: &str) -> String {
    format!("{prefix}_shown")
}

/// 폼 값을 서비스 입력으로. 변환·검사는 core `fields::validate` 가 한다.
pub fn collect(
    schema: &[FieldDef],
    form: &HashMap<String, String>,
    prefix: &str,
) -> Option<Map<String, Value>> {
    if !form.contains_key(&shown_name(prefix)) {
        return None;
    }
    let mut out = Map::new();
    for d in schema {
        let name = format!("{prefix}{}", d.key);
        match d.kind.as_str() {
            "multiselect" => {
                let picked = d
                    .options
                    .iter()
                    .filter(|o| form.contains_key(&format!("{name}.{}", o.value)))
                    .map(|o| Value::String(o.value.clone()))
                    .collect();
                out.insert(d.key.clone(), Value::Array(picked));
            }
            "boolean" => {
                out.insert(d.key.clone(), Value::Bool(form.contains_key(&name)));
            }
            _ => {
                if let Some(v) = form.get(&name) {
                    out.insert(d.key.clone(), Value::String(v.clone()));
                }
            }
        }
    }
    Some(out)
}

fn as_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// 화면에 그릴 칸. `values` 는 저장된 값이거나 방금 보낸 값.
pub fn view(schema: &[FieldDef], values: &Map<String, Value>, prefix: &str) -> Vec<Value> {
    schema
        .iter()
        .map(|d| {
            let name = format!("{prefix}{}", d.key);
            let v = values.get(&d.key);
            let picked = |value: &str| match v {
                Some(Value::Array(a)) => a.iter().any(|x| x.as_str() == Some(value)),
                Some(Value::String(s)) => s == value,
                _ => false,
            };
            let input = match d.kind.as_str() {
                "date" => "date",
                "datetime" => "datetime-local",
                "url" => "url",
                "email" => "email",
                "phone" => "tel",
                _ => "text",
            };
            json!({
                "key": d.key,
                "label": d.label,
                "type": d.kind,
                "input": input,
                "required": d.required,
                "help": d.help,
                "name": name,
                "id": format!("f-{}", name.replace('.', "-")),
                "value": as_text(v),
                "checked": matches!(v, Some(Value::Bool(true))),
                "options": d.options.iter().map(|o| json!({
                    "value": o.value,
                    "label": if o.label.is_empty() { &o.value } else { &o.label },
                    "name": format!("{name}.{}", o.value),
                    "checked": picked(&o.value),
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use maru_core::fields::{FieldOption, check_schema};

    fn schema() -> Vec<FieldDef> {
        let d = |key: &str, kind: &str| FieldDef {
            key: key.into(),
            label: key.into(),
            kind: kind.into(),
            required: false,
            options: if kind.ends_with("select") {
                vec![
                    FieldOption {
                        value: "a".into(),
                        label: String::new(),
                    },
                    FieldOption {
                        value: "b".into(),
                        label: "비".into(),
                    },
                ]
            } else {
                Vec::new()
            },
            help: None,
            list: false,
            filter: false,
        };
        check_schema(vec![
            d("price", "number"),
            d("ok", "boolean"),
            d("tags", "multiselect"),
        ])
        .unwrap()
    }

    #[test]
    fn collect_reads_marker_checkboxes_and_options() {
        let p = prefix(Some("free"));
        let mut form: HashMap<String, String> = HashMap::new();
        assert!(
            collect(&schema(), &form, &p).is_none(),
            "칸을 그리지 않았으면 건드리지 않는다"
        );
        form.insert(shown_name(&p), "1".into());
        form.insert(format!("{p}price"), "1,000".into());
        form.insert(format!("{p}tags.b"), "on".into());
        let m = collect(&schema(), &form, &p).unwrap();
        assert_eq!(m["price"], "1,000");
        assert_eq!(m["ok"], false);
        assert_eq!(m["tags"], json!(["b"]));
        let v = view(&schema(), &m, &p);
        assert_eq!(v[2]["options"][1]["checked"], true);
        assert_eq!(v[2]["options"][1]["label"], "비");
        assert_eq!(v[0]["id"], "f-field-free-price");
    }
}
