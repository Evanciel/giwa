//! 섹션 블록 화면 값 (ADR-0125). 저장된 설정을 템플릿 값으로 바꾼다.
//!
//! 칸 이름으로 일을 나눈다: `image` 는 그림 정보로, `link` 는 주소 객체로, 빈 단추는 없는 것으로,
//! 마크다운 칸은 정화한 HTML 로. 종류마다 같은 일을 다시 쓰지 않으려는 것이다.

use std::collections::HashMap;

use maru_core::directives;
use maru_core::model::SiteSettings;
use maru_core::services::{media, sections};
use maru_core::{Ctx, Result, clock, markdown};
use serde_json::{Value, json};

/// 마크다운을 받는 칸. 그려서 `<칸>_html` 로 둔다.
const MARKDOWN_FIELDS: [&str; 2] = ["a", "body"];

pub(crate) async fn view(
    ctx: &Ctx,
    site: &SiteSettings,
    kind: &str,
    config: &Value,
) -> Result<Value> {
    let mut data = config.clone();
    let mut images = Vec::new();
    collect_images(&data, &mut images);
    let mut infos = HashMap::new();
    for src in images {
        if let Some(info) = image_json(ctx, &src).await? {
            infos.insert(src, info);
        }
    }
    // 섹션 배경 그림 (ADR-0157). `image` 칸이 아니라 공통 칸이라 따로 읽는다. 지운 그림이면 배경을 그리지 않는다.
    let bg = data["bg_image"].as_str().unwrap_or("").to_owned();
    if !bg.is_empty() {
        data["bg"] = image_json(ctx, &bg).await?.unwrap_or(Value::Null);
    }
    let mut body_images = HashMap::new();
    let mut bodies = Vec::new();
    collect_markdown(&data, &mut bodies);
    for md in bodies {
        body_images.extend(media::images_in(ctx, &md).await?);
    }
    fill(&mut data, &infos, &body_images);
    match kind {
        "hero" => {
            if data["title"].as_str().is_none_or(str::is_empty) {
                data["title"] = json!(site.name);
            }
            if data["text"].as_str().is_none_or(str::is_empty) {
                data["text"] = json!(site.description);
            }
        }
        "visit" => {
            let (naver, kakao) =
                directives::map_search_urls(data["address"].as_str().unwrap_or(""));
            data["naver_url"] = json!(naver);
            data["kakao_url"] = json!(kakao);
            let tel: String = data["phone"]
                .as_str()
                .unwrap_or("")
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == '+')
                .collect();
            data["tel"] = json!(tel);
        }
        "video" => {
            data["embed"] = json!(directives::video_embed(data["url"].as_str().unwrap_or("")));
        }
        "gallery" => {
            // 그림을 하나도 고르지 않은 사진 전시는 공개 화면에서 뺀다(편집 중엔 고르기 칸을 보인다).
            let any = data["items"]
                .as_array()
                .is_some_and(|a| a.iter().any(|i| i["image"].is_object()));
            data["empty"] = json!(!any);
        }
        "countdown" => {
            // 날짜 수(D-day)는 서버가 그리고, 시·분은 site.js 가 있으면 채운다.
            let target = data["target"].as_str().unwrap_or("").to_owned();
            data["passed"] = json!(clock::seconds_since(&target).is_none_or(|s| s >= 0));
            data["days"] = json!(clock::days_until(&target, site.tz_offset_min).unwrap_or(0));
            data["at"] = json!(clock::display_datetime(&target, site.tz_offset_min));
        }
        "social" => {
            if let Some(items) = data["items"].as_array_mut() {
                for i in items {
                    if i["label"].as_str().is_none_or(str::is_empty) {
                        let name = sections::SOCIAL_SERVICES
                            .iter()
                            .find(|(id, _)| i["service"] == *id)
                            .map_or("", |(_, n)| n);
                        i["label"] = json!(name);
                    }
                    let url = i["url"].as_str().unwrap_or("");
                    i["external"] = json!(url.starts_with("http"));
                }
            }
        }
        _ => {}
    }
    Ok(data)
}

/// 보관소 그림의 화면 값(`src`·`srcset`·크기). 보관소에 없으면 `None`.
async fn image_json(ctx: &Ctx, src: &str) -> Result<Option<Value>> {
    let Some(info) = media::image_info(ctx, src).await? else {
        return Ok(None);
    };
    let srcset = info
        .variants
        .iter()
        .map(|(width, u)| format!("{u} {width}w"))
        .chain(std::iter::once(format!("{src} {}w", info.width)))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(Some(
        json!({ "src": src, "srcset": srcset, "width": info.width, "height": info.height }),
    ))
}

fn collect_images(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                match (k.as_str(), x) {
                    ("image", Value::String(s)) if !s.is_empty() && !out.contains(s) => {
                        out.push(s.clone());
                    }
                    _ => collect_images(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_images(x, out)),
        _ => {}
    }
}

fn collect_markdown(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                match x {
                    Value::String(s) if MARKDOWN_FIELDS.contains(&k.as_str()) => {
                        out.push(s.clone())
                    }
                    _ => collect_markdown(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_markdown(x, out)),
        _ => {}
    }
}

fn fill(
    v: &mut Value,
    infos: &HashMap<String, Value>,
    body_images: &HashMap<String, markdown::ImageInfo>,
) {
    match v {
        Value::Object(m) => {
            let mut html = Vec::new();
            for (k, x) in m.iter_mut() {
                match (k.as_str(), &*x) {
                    ("image", Value::String(s)) => {
                        *x = infos.get(s).cloned().unwrap_or(Value::Null);
                    }
                    ("link", Value::String(s)) => {
                        *x = crate::widgets::link_json(s);
                    }
                    ("button" | "button2", Value::Object(b)) => {
                        let empty = b
                            .get("label")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty);
                        if empty {
                            *x = Value::Null;
                        } else {
                            fill(x, infos, body_images);
                        }
                    }
                    (name, Value::String(s)) if MARKDOWN_FIELDS.contains(&name) => {
                        let rendered = markdown::render_with(s, &|u| body_images.get(u).cloned());
                        html.push((format!("{name}_html"), rendered.into_string()));
                    }
                    _ => fill(x, infos, body_images),
                }
            }
            for (k, h) in html {
                m.insert(k, json!(h));
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| fill(x, infos, body_images)),
        _ => {}
    }
}
