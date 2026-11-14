//! 검색 노출 보강 (ADR-0143): 설명·공유 그림 채우기, schema.org 구조화 데이터, 공유 그림, IndexNow 키 파일.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use giwa_core::sample_art;
use giwa_core::services::{design, indexnow};
use serde_json::{Value, json};

use crate::AppState;

/// 서버가 그린 공유 그림의 주소.
pub(crate) const SHARE_PATH: &str = "/share.jpg";

/// 화면 값에 빠진 설명·공유 그림을 채우고 구조화 데이터를 붙인다. 테마는 `meta`·`seo` 를 찍기만 한다.
pub(crate) fn decorate(site_url: &str, name: &str, v: &mut Value) {
    if !v["meta"].is_object() {
        return;
    }
    if text(&v["meta"]["description"]).is_none() {
        let fallback = text(&v["board"]["description"])
            .or_else(|| text(&v["site"]["description"]))
            .map(str::to_owned);
        if let Some(d) = fallback {
            v["meta"]["description"] = json!(d);
        }
    }
    if text(&v["meta"]["image"]).is_none() {
        match text(&v["site"]["share_image"]) {
            Some(own) => v["meta"]["image"] = json!(absolute(site_url, own)),
            None => {
                v["meta"]["image"] = json!(absolute(site_url, SHARE_PATH));
                v["meta"]["image_width"] = json!(sample_art::SHARE_WIDTH);
                v["meta"]["image_height"] = json!(sample_art::SHARE_HEIGHT);
            }
        }
    }
    if v["meta"]["noindex"] == true {
        return;
    }
    let graph = graph(site_url, name, v);
    if !graph.is_empty() {
        let doc = json!({ "@context": "https://schema.org", "@graph": graph });
        v["seo"] = json!({ "jsonld": script_json(&doc) });
    }
}

fn text(v: &Value) -> Option<&str> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn absolute(site_url: &str, url: &str) -> String {
    if url.starts_with('/') {
        format!("{}{url}", site_url.trim_end_matches('/'))
    } else {
        url.to_owned()
    }
}

/// `<script>` 블록 안에 둘 JSON. `<` `>` `&` 를 바꿔 블록을 끝내거나 HTML 로 읽히지 않게 한다.
pub(crate) fn script_json(v: &Value) -> String {
    v.to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn graph(site_url: &str, name: &str, v: &Value) -> Vec<Value> {
    let site = site_url.trim_end_matches('/');
    let home = format!("{site}/");
    let site_name = text(&v["site"]["name"]).unwrap_or("Giwa");
    let org_id = format!("{home}#organization");
    let lang = text(&v["site"]["lang"]).unwrap_or("ko");
    let canonical = text(&v["canonical"]).map(str::to_owned);
    let crumb = |items: Vec<(String, String)>| {
        json!({
            "@type": "BreadcrumbList",
            "itemListElement": items.into_iter().enumerate().map(|(i, (n, u))| json!({
                "@type": "ListItem",
                "position": i + 1,
                "name": n,
                "item": u,
            })).collect::<Vec<_>>(),
        })
    };
    let home_item = (site_name.to_owned(), home.clone());
    let is_home = name == "home.html" || (name == "page.html" && v["is_home"] == true);
    if is_home {
        let mut website = json!({
            "@type": "WebSite",
            "@id": format!("{home}#website"),
            "url": home,
            "name": site_name,
            "inLanguage": lang,
            "publisher": { "@id": org_id },
        });
        if let Some(d) = text(&v["meta"]["description"]) {
            website["description"] = json!(d);
        }
        let org = json!({
            "@type": "Organization",
            "@id": org_id,
            "name": site_name,
            "url": home,
        });
        return vec![website, org];
    }
    match name {
        "post.html" => {
            let title = text(&v["post"]["title"]).unwrap_or(site_name);
            let board_name = text(&v["board"]["name"]).unwrap_or_default();
            let board_url = text(&v["board"]["url"]).map(|u| absolute(site, u));
            let mut article = json!({
                "@type": "Article",
                "headline": title.chars().take(110).collect::<String>(),
                "inLanguage": lang,
                "publisher": { "@type": "Organization", "@id": org_id, "name": site_name, "url": home },
            });
            if let Some(u) = &canonical {
                article["mainEntityOfPage"] = json!(u);
                article["url"] = json!(u);
            }
            for (key, from) in [
                ("description", "description"),
                ("datePublished", "published_time"),
                ("dateModified", "modified_time"),
            ] {
                if let Some(t) = text(&v["meta"][from]) {
                    article[key] = json!(t);
                }
            }
            if let Some(img) = text(&v["meta"]["image"]) {
                article["image"] = json!([img]);
            }
            article["author"] = match text(&v["post"]["author"]["name"]) {
                Some(n) => {
                    let mut p = json!({ "@type": "Person", "name": n });
                    if let Some(u) = text(&v["post"]["author"]["url"]) {
                        p["url"] = json!(absolute(site, u));
                    }
                    p
                }
                None => json!({ "@type": "Organization", "name": site_name, "url": home }),
            };
            let mut items = vec![home_item];
            if let Some(u) = board_url {
                items.push((board_name.to_owned(), u));
            }
            if let Some(u) = canonical {
                items.push((title.to_owned(), u));
            }
            vec![article, crumb(items)]
        }
        "board.html" => match (text(&v["board"]["name"]), canonical) {
            (Some(n), Some(u)) => vec![crumb(vec![home_item, (n.to_owned(), u)])],
            _ => Vec::new(),
        },
        "page.html" => match (text(&v["page"]["title"]), canonical) {
            (Some(t), Some(u)) => vec![crumb(vec![home_item, (t.to_owned(), u)])],
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// 사이트 공유 그림 `/share.jpg`: 사이트 주 색의 추상 장면, 1200×630. 색이 같으면 그려 둔 것을 쓴다.
pub(crate) async fn share_picture(State(state): State<AppState>) -> Response {
    let primary = design::tokens(&state.ctx)
        .await
        .map(|t| t.color.primary)
        .unwrap_or_default();
    let cached = state.share_picture.lock().ok().and_then(|c| {
        c.as_ref()
            .filter(|(k, _)| *k == primary)
            .map(|(_, b)| b.clone())
    });
    let bytes = match cached {
        Some(b) => b,
        None => {
            let seed = primary
                .bytes()
                .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
            let color = primary.clone();
            let drawn = tokio::task::spawn_blocking(move || sample_art::share(seed, &color)).await;
            let Ok(Ok(jpeg)) = drawn else {
                return crate::error::page(
                    &state,
                    &giwa_core::Error::new(giwa_core::Code::Internal),
                )
                .await;
            };
            let b = Arc::new(jpeg);
            if let Ok(mut c) = state.share_picture.lock() {
                *c = Some((primary, b.clone()));
            }
            b
        }
    };
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        bytes.as_ref().clone(),
    )
        .into_response()
}

/// 사이트 맨 위의 한 조각 주소. IndexNow 키 파일(`/{키}.txt`)만 답하고 나머지는 없는 주소다.
pub(crate) async fn root_file(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    if let Some(stem) = file.strip_suffix(".txt")
        && let Ok(Some(key)) = indexnow::key(&state.ctx).await
        && stem == key
    {
        return (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            key,
        )
            .into_response();
    }
    crate::error::page(&state, &giwa_core::Error::new(giwa_core::Code::NotFound)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_json_cannot_close_the_block() {
        let s = script_json(&json!({ "name": "</script><b>&" }));
        assert!(!s.contains('<') && !s.contains('>') && !s.contains('&'));
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["name"], "</script><b>&");
    }

    #[test]
    fn missing_description_and_image_are_filled() {
        let mut v = json!({
            "meta": { "title": "자유게시판 · 동네", "description": "" },
            "site": { "name": "동네", "description": "동네 소식", "share_image": "" },
            "board": { "name": "자유게시판", "url": "/b/free", "description": "" },
            "canonical": "https://a.co.kr/b/free",
        });
        decorate("https://a.co.kr", "board.html", &mut v);
        assert_eq!(v["meta"]["description"], "동네 소식");
        assert_eq!(v["meta"]["image"], "https://a.co.kr/share.jpg");
        assert_eq!(v["meta"]["image_width"], 1200);
        let ld: Value = serde_json::from_str(v["seo"]["jsonld"].as_str().unwrap()).unwrap();
        assert_eq!(ld["@graph"][0]["@type"], "BreadcrumbList");
        assert_eq!(
            ld["@graph"][0]["itemListElement"][1]["item"],
            "https://a.co.kr/b/free"
        );
    }

    #[test]
    fn noindex_screens_get_no_structured_data() {
        let mut v =
            json!({ "meta": { "title": "검색", "noindex": true }, "site": { "name": "동네" } });
        decorate("https://a.co.kr", "search.html", &mut v);
        assert!(v["seo"].is_null());
    }
}
