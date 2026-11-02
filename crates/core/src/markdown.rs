//! Markdown 렌더링. 결과는 반드시 ammonia 로 정화하고 [`SafeHtml`] 로만 내보낸다 (PHASE1_PLAN §7).

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use serde::Serialize;

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

pub fn render(md: &str) -> SafeHtml {
    let mut raw = String::with_capacity(md.len() * 3 / 2);
    html::push_html(&mut raw, Parser::new_ext(md, options()));
    let clean = ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .add_tags(["input"])
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .clean(&raw)
        .to_string();
    SafeHtml(clean)
}

/// 목록·메타 설명용 평문 요약. `max_chars` 를 넘으면 `…` 로 자른다.
pub fn plain_text(md: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for event in Parser::new_ext(md, options()) {
        match event {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak => out.push(' '),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => out.push(' '),
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
    }
}
