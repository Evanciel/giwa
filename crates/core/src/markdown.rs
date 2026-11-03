//! Markdown 렌더링. 결과는 반드시 ammonia 로 정화하고 [`SafeHtml`] 로만 내보낸다 (PHASE1_PLAN §7).

use std::collections::HashMap;

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use serde::Serialize;

use crate::directives;

/// 정화를 거친 HTML. 이 모듈 밖에서는 만들 수 없다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SafeHtml(String);

impl SafeHtml {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// 보관소에 있는 그림의 크기와 축소본. 공개 화면이 `width`·`height`·`srcset` 을 붙이는 데 쓴다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// (폭, 주소). 주소는 `/media/` 로 시작해야 정화 단계를 통과한다.
    pub variants: Vec<(u32, String)>,
}

/// 사진 크기 단계(%). 100% 는 조각 없이 쓴다. 에디터의 `IMAGE_WIDTHS` 와 같아야 한다.
pub const IMAGE_WIDTHS: [u32; 2] = [75, 50];

/// 축소본이 있을 때 브라우저에 알려 줄 표시 폭. 본문 폭보다 넓게 그리는 일은 없다.
const IMAGE_SIZES: &str = "(max-width: 720px) 100vw, 720px";

/// 본문에 쓰인 그림 주소. 크기 조각(`#w50`)은 뗀다. 그림 정보를 미리 찾아 둘 때 쓴다.
pub fn image_urls(md: &str) -> Vec<String> {
    let md = &*directives::expand(md, options());
    let mut out: Vec<String> = Vec::new();
    for event in Parser::new_ext(md, options()) {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            let (url, _) = split_width(&dest_url);
            if !out.iter().any(|u| u == url) {
                out.push(url.to_owned());
            }
        }
    }
    out
}

/// 주소 끝의 크기 조각. `사진.jpg#w50` → (`사진.jpg`, Some(50)). 정해진 값만 받는다.
fn split_width(url: &str) -> (&str, Option<u32>) {
    for w in IMAGE_WIDTHS {
        if let Some(base) = url.strip_suffix(&format!("#w{w}")) {
            return (base, Some(w));
        }
    }
    (url, None)
}

const TABLE_ALIGNS: [&str; 3] = ["left", "center", "right"];

pub fn render(md: &str) -> SafeHtml {
    render_with(md, &|_| None)
}

/// [`render`] 와 같고, 보관소 그림에는 `images` 가 돌려준 크기·축소본을 붙인다.
pub fn render_with(md: &str, images: &dyn Fn(&str) -> Option<ImageInfo>) -> SafeHtml {
    let md = &*directives::expand(md, options());
    let mut raw = String::with_capacity(md.len() * 3 / 2);
    // 각주는 직접 그린다. pulldown-cmark 기본 출력은 라벨을 그대로 id 로 쓰는데(`id="메모"`),
    // 정화 단계에서 임의의 id 를 허용할 수 없어 번호 링크가 끊긴다. 번호로 만든 `fn-N` 만 쓴다.
    let mut numbers: HashMap<String, usize> = HashMap::new();
    let events = Parser::new_ext(md, options()).map(|event| {
        let mut number = |label: &str| {
            let next = numbers.len() + 1;
            *numbers.entry(label.to_owned()).or_insert(next)
        };
        match event {
            Event::FootnoteReference(label) => {
                let n = number(&label);
                Event::InlineHtml(
                    format!("<sup class=\"fnref\" id=\"fnref-{n}\"><a href=\"#fn-{n}\">{n}</a></sup>")
                        .into(),
                )
            }
            Event::Start(Tag::FootnoteDefinition(label)) => {
                let n = number(&label);
                Event::Html(
                    format!(
                        "<div class=\"footnote\" id=\"fn-{n}\"><a class=\"fnback\" href=\"#fnref-{n}\">{n}</a>"
                    )
                    .into(),
                )
            }
            Event::End(TagEnd::FootnoteDefinition) => Event::Html("</div>\n".into()),
            other => other,
        }
    });
    let events = images_to_html(events.collect(), images);
    html::push_html(&mut raw, events.into_iter());
    // 표 열 정렬: pulldown-cmark 는 style 속성으로 쓰는데 정화 단계는 style 을 모두 지운다. 클래스로 바꾼다.
    for a in TABLE_ALIGNS {
        raw = raw.replace(
            &format!(" style=\"text-align: {a}\">"),
            &format!(" class=\"ta-{a}\">"),
        );
    }
    let mut builder = ammonia::Builder::default();
    for tag in ["th", "td"] {
        builder.add_allowed_classes(
            tag,
            TABLE_ALIGNS.map(|a| match a {
                "center" => "ta-center",
                "right" => "ta-right",
                _ => "ta-left",
            }),
        );
    }
    // 확장 문법이 만든 클래스(강조 상자·정렬·글자 꾸밈)만 남긴다.
    for (tag, classes) in directives::allowed_classes() {
        builder.add_allowed_classes(tag, classes.iter().map(String::as_str));
    }
    let clean = builder
        .link_rel(Some("noopener noreferrer"))
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .add_tags(["input"])
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .add_tag_attributes("sup", ["id"])
        .add_tag_attributes("div", ["id"])
        .add_tag_attributes("img", ["srcset", "sizes", "loading", "decoding"])
        .add_allowed_classes("img", IMAGE_WIDTHS.map(width_class))
        .add_allowed_classes("figure", ["figure"])
        .add_allowed_classes("figure", IMAGE_WIDTHS.map(width_class))
        .add_allowed_classes("sup", ["fnref"])
        .add_allowed_classes("div", ["footnote"])
        .add_allowed_classes("a", ["fnback"])
        // id 는 각주 번호(`fn-3`, `fnref-3`)만 남긴다. 사용자가 쓴 다른 id 는 DOM 을 어지럽힐 수 있다.
        .attribute_filter(|_, attr, value| {
            let ok = match attr {
                "id" => is_note_id(value),
                // 축소본 목록은 이 모듈이 보관소 주소로만 만든다. 사용자가 쓴 srcset 은 지운다.
                "srcset" => is_media_srcset(value),
                "sizes" => value == IMAGE_SIZES,
                "loading" => value == "lazy",
                "decoding" => value == "async",
                _ => true,
            };
            ok.then(|| value.into())
        })
        .clean(&raw)
        .to_string();
    SafeHtml(directives::embed_videos(&clean))
}

fn width_class(w: u32) -> &'static str {
    match w {
        75 => "img-w75",
        _ => "img-w50",
    }
}

fn is_media_srcset(v: &str) -> bool {
    !v.is_empty()
        && v.split(", ").all(|cand| {
            let Some((url, w)) = cand.rsplit_once(' ') else {
                return false;
            };
            url.starts_with("/media/")
                && url
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/-._".contains(&b))
                && w.strip_suffix('w')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// 그림을 직접 그린다. 크기 조각을 클래스로 바꾸고, 보관소 그림에는 크기·축소본을 붙이며,
/// 그림 하나만 있는 문단에 제목이 있으면 `<figure>` 와 캡션으로 감싼다.
fn images_to_html<'a>(
    events: Vec<Event<'a>>,
    images: &dyn Fn(&str) -> Option<ImageInfo>,
) -> Vec<Event<'a>> {
    let mut out = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        // 캡션 문단 = [Start(Paragraph), Start(Image), 글자…, End(Image), End(Paragraph)]
        if matches!(events[i], Event::Start(Tag::Paragraph))
            && let Some(Event::Start(Tag::Image { title, .. })) = events.get(i + 1)
            && !title.trim().is_empty()
            && let Some(end) = image_end(&events, i + 1)
            && matches!(events.get(end + 1), Some(Event::End(TagEnd::Paragraph)))
        {
            let (img, width) = image_html(&events[i + 1..=end], images, false);
            let class = match width {
                Some(w) => format!("figure {}", width_class(w)),
                None => "figure".to_owned(),
            };
            out.push(Event::Html(
                format!(
                    "<figure class=\"{class}\">{img}<figcaption>{}</figcaption></figure>\n",
                    escape(title)
                )
                .into(),
            ));
            i = end + 2;
            continue;
        }
        if matches!(events[i], Event::Start(Tag::Image { .. }))
            && let Some(end) = image_end(&events, i)
        {
            let (img, _) = image_html(&events[i..=end], images, true);
            out.push(Event::InlineHtml(img.into()));
            i = end + 1;
            continue;
        }
        out.push(events[i].clone());
        i += 1;
    }
    out
}

fn image_end(events: &[Event<'_>], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, e) in events.iter().enumerate().skip(start) {
        match e {
            Event::Start(Tag::Image { .. }) => depth += 1,
            Event::End(TagEnd::Image) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// `<img>` 하나. `inline` 이면 크기 클래스와 제목을 그림에 붙인다(캡션 상자는 상자에 붙인다).
fn image_html(
    events: &[Event<'_>],
    images: &dyn Fn(&str) -> Option<ImageInfo>,
    inline: bool,
) -> (String, Option<u32>) {
    let (dest, title) = match events.first() {
        Some(Event::Start(Tag::Image {
            dest_url, title, ..
        })) => (dest_url.clone(), title.clone()),
        _ => (CowStr::Borrowed(""), CowStr::Borrowed("")),
    };
    let mut alt = String::new();
    for e in events.iter().skip(1) {
        if let Event::Text(t) | Event::Code(t) = e {
            alt.push_str(t);
        }
    }
    let (src, width) = split_width(&dest);
    let mut html = format!("<img src=\"{}\" alt=\"{}\"", escape(src), escape(&alt));
    if inline && !title.is_empty() {
        html.push_str(&format!(" title=\"{}\"", escape(&title)));
    }
    if let Some(w) = width.filter(|_| inline) {
        html.push_str(&format!(" class=\"{}\"", width_class(w)));
    }
    if let Some(info) = images(src) {
        if info.width > 0 && info.height > 0 {
            html.push_str(&format!(
                " width=\"{}\" height=\"{}\"",
                info.width, info.height
            ));
        }
        if !info.variants.is_empty() {
            let mut set: Vec<String> = info
                .variants
                .iter()
                .map(|(w, u)| format!("{u} {w}w"))
                .collect();
            set.push(format!("{src} {}w", info.width));
            html.push_str(&format!(
                " srcset=\"{}\" sizes=\"{IMAGE_SIZES}\"",
                escape(&set.join(", "))
            ));
        }
    }
    html.push_str(" loading=\"lazy\" decoding=\"async\">");
    (html, width)
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

fn is_note_id(v: &str) -> bool {
    let digits = v
        .strip_prefix("fnref-")
        .or_else(|| v.strip_prefix("fn-"))
        .unwrap_or("");
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// 요약을 비워 둔 글의 목록·메타 설명·공유 미리보기 글자 수.
pub const SUMMARY_CHARS: usize = 160;

/// 목록·메타 설명용 평문 요약. `max_chars` 를 넘으면 `…` 로 자른다.
pub fn plain_text(md: &str, max_chars: usize) -> String {
    let md = &*directives::expand(md, options());
    let mut out = String::new();
    // 그림 설명(alt)은 요약에 넣지 않는다.
    let mut in_image = 0usize;
    for event in Parser::new_ext(md, options()) {
        match event {
            Event::Start(Tag::Image { .. }) => in_image += 1,
            Event::End(TagEnd::Image) => in_image = in_image.saturating_sub(1),
            Event::Text(t) | Event::Code(t) if in_image == 0 => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::TableHead
                | TagEnd::TableRow,
            ) => out.push(' '),
            Event::Start(Tag::CodeBlock(_)) => out.push(' '),
            _ => {}
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max_chars).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hangul_and_basic_markup() {
        let html = render("# 제목\n\n**굵게** 그리고 `코드`\n\n- [x] 할 일");
        let s = html.as_str();
        assert!(s.contains("<h1>제목</h1>"), "{s}");
        assert!(s.contains("<strong>굵게</strong>"));
        assert!(s.contains("<code>코드</code>"));
        assert!(s.contains("checkbox"));
    }

    #[test]
    fn xss_vectors_are_removed() {
        let vectors = [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "[클릭](javascript:alert(1))",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<iframe src=\"https://evil.example\"></iframe>",
            "<svg onload=alert(1)>",
            "<div style=\"background:url(javascript:alert(1))\">x</div>",
            "[x](data:text/html;base64,PHNjcmlwdD4=)",
        ];
        for v in vectors {
            let s = render(v).into_string().to_ascii_lowercase();
            for bad in [
                "<script",
                "onerror",
                "javascript:",
                "<iframe",
                "onload",
                "data:text",
                "style=",
            ] {
                assert!(!s.contains(bad), "{v} -> {s}");
            }
        }
    }

    #[test]
    fn footnotes_link_by_number_and_other_ids_are_dropped() {
        let s =
            render("바다[^메모] 산[^2]\n\n[^메모]: 동해안\n\n[^2]: 설악산\n\n<p id=\"x\">a</p>")
                .into_string();
        assert!(
            s.contains("<sup class=\"fnref\" id=\"fnref-1\"><a href=\"#fn-1\""),
            "{s}"
        );
        assert!(s.contains("<div class=\"footnote\" id=\"fn-1\">"), "{s}");
        assert!(s.contains("href=\"#fnref-2\""), "{s}");
        assert!(s.contains("설악산"), "{s}");
        assert!(!s.contains("id=\"x\""), "{s}");
        assert!(!s.contains("메모\""), "{s}");
    }

    #[test]
    fn directive_classes_survive_cleaning_and_nothing_else_does() {
        let s = render(":::warn\n**조심** :color[빨강]{red}\n:::\n\n<div class=\"evil callout\" onclick=\"x()\">a</div> <span class=\"tc-red other\">b</span>")
            .into_string();
        assert!(s.contains("<div class=\"callout callout-warn\">"), "{s}");
        assert!(s.contains("<strong>조심</strong>"), "{s}");
        assert!(s.contains("<span class=\"tc-red\">빨강</span>"), "{s}");
        assert!(
            !s.contains("evil") && !s.contains("onclick") && !s.contains("other"),
            "{s}"
        );
        assert_eq!(
            plain_text(":::info\n알림 :mark[중요]{yellow}\n:::", 50),
            "알림 중요"
        );
    }

    #[test]
    fn image_width_caption_and_srcset() {
        let info = |url: &str| {
            (url == "/media/2026/11/abc.jpg").then(|| ImageInfo {
                width: 1500,
                height: 1000,
                variants: vec![(640, "/media/2026/11/abc-640.jpg".to_owned())],
            })
        };
        let s = render_with(
            "![바다](/media/2026/11/abc.jpg#w50 \"동해 바다\")\n\n글 ![산](https://example.com/x.png#w75) 끝",
            &info,
        )
        .into_string();
        assert!(s.contains("<figure class=\"figure img-w50\">"), "{s}");
        assert!(s.contains("<figcaption>동해 바다</figcaption>"), "{s}");
        assert!(s.contains("width=\"1500\" height=\"1000\""), "{s}");
        assert!(
            s.contains("srcset=\"/media/2026/11/abc-640.jpg 640w, /media/2026/11/abc.jpg 1500w\""),
            "{s}"
        );
        assert!(s.contains("src=\"https://example.com/x.png\""), "{s}");
        assert!(s.contains("class=\"img-w75\""), "{s}");
        assert!(!s.contains("#w"), "{s}");
        assert_eq!(
            image_urls("![a](/media/a.jpg#w50) ![b](/media/a.jpg) ![c](/m/c.png)"),
            ["/media/a.jpg", "/m/c.png"]
        );
    }

    #[test]
    fn user_written_srcset_and_attrs_are_dropped() {
        let s = render(
            "<img src=\"/a.png\" srcset=\"https://evil.example/x.png 1w\" sizes=\"1px\" loading=\"eager\" class=\"img-w50 x\">",
        )
        .into_string();
        assert!(
            !s.contains("evil") && !s.contains("sizes") && !s.contains("eager"),
            "{s}"
        );
        assert!(s.contains("class=\"img-w50\""), "{s}");
        let g = render(":::gallery\n![a](/a.png) ![b](/b.png)\n:::").into_string();
        assert!(g.contains("<div class=\"gallery\">"), "{g}");
    }

    #[test]
    fn table_alignment_becomes_class() {
        let s = render("| a | b | c |\n| :--- | :---: | ---: |\n| 1 | 2 | 3 |").into_string();
        assert!(s.contains("<th class=\"ta-left\">a</th>"), "{s}");
        assert!(s.contains("<td class=\"ta-center\">2</td>"), "{s}");
        assert!(s.contains("<td class=\"ta-right\">3</td>"), "{s}");
        assert!(!s.contains("style="), "{s}");
    }

    #[test]
    fn links_get_rel() {
        let s = render("[블로그](https://example.com)").into_string();
        assert!(s.contains("rel=\"noopener noreferrer\""), "{s}");
    }

    #[test]
    fn plain_text_summary() {
        assert_eq!(
            plain_text("# 안녕\n\n첫 **문단**입니다.", 100),
            "안녕 첫 문단입니다."
        );
        assert_eq!(plain_text("가나다라마바사", 3), "가나다…");
        // 표 칸은 띄어 쓰고, 그림 설명은 뺀다
        assert_eq!(
            plain_text(
                "여행 ![바다 풍경](/media/a.jpg)\n\n| 항목 | 금액 |\n|---|---|\n| 숙소 | 12만 |",
                100
            ),
            "여행 항목 금액 숙소 12만"
        );
    }
}
