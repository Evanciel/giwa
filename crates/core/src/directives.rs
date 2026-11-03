//! Maru 확장 문법 (ADR-0034). Markdown 을 pulldown-cmark 에 넘기기 전에 HTML 조각으로 바꾼다.
//!
//! - 블록 상자: `:::info` … `:::` (info·tip·warn·danger 강조 상자, center·right 정렬, gallery 사진 격자,
//!   details 접기 — 첫 문단이 요약, ADR-0041)
//! - 글자 꾸밈: `:color[글자]{red}`, `:mark[글자]{yellow}`, `:size[글자]{large}`
//!
//! 에디터(`editor/src/syntax.ts`)와 같은 규칙이어야 한다. 정해진 이름·값만 받고, 나머지는 글자 그대로 둔다.
//! 만든 HTML 은 다른 HTML 과 똑같이 ammonia 정화를 거친다 (허용 클래스는 [`allowed_classes`]).

use std::borrow::Cow;
use std::ops::Range;
use std::sync::LazyLock;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub const BOX_KINDS: [&str; 8] = [
    "info", "tip", "warn", "danger", "center", "right", "gallery", "details",
];

/// (문법 이름, 클래스 앞머리, 쓸 수 있는 값)
pub const SPANS: [(&str, &str, &[&str]); 3] = [
    (
        "color",
        "tc",
        &["red", "orange", "green", "blue", "purple", "gray"],
    ),
    ("mark", "hl", &["yellow", "green", "blue", "pink"]),
    ("size", "fs", &["small", "large", "xlarge"]),
];

fn box_class(kind: &str) -> String {
    match kind {
        "center" | "right" => format!("align-{kind}"),
        "gallery" => "gallery".to_owned(),
        "details" => "fold".to_owned(),
        _ => format!("callout callout-{kind}"),
    }
}

/// 정화 단계에서 허용할 (태그, 클래스) 목록.
/// ammonia 빌더가 `'static` 문자열을 요구해서 한 번만 만들어 둔다.
pub fn allowed_classes() -> &'static [(&'static str, Vec<String>)] {
    static CLASSES: LazyLock<Vec<(&'static str, Vec<String>)>> = LazyLock::new(build_classes);
    &CLASSES
}

fn build_classes() -> Vec<(&'static str, Vec<String>)> {
    let mut divs = vec!["callout".to_owned()];
    // 접기 상자는 div 가 아니라 <details> 로 그린다.
    for k in BOX_KINDS.into_iter().filter(|k| *k != "details") {
        divs.extend(
            box_class(k)
                .split(' ')
                .filter(|c| *c != "callout")
                .map(str::to_owned),
        );
    }
    let mut spans: Vec<String> = SPANS
        .iter()
        .flat_map(|(_, prefix, values)| values.iter().map(move |v| format!("{prefix}-{v}")))
        .collect();
    divs.extend(["video", "place"].map(str::to_owned));
    spans.extend(["place-addr", "place-links"].map(str::to_owned));
    vec![
        ("div", divs),
        ("span", spans),
        ("strong", vec!["place-name".to_owned()]),
        ("details", vec![box_class("details")]),
    ]
}

pub fn expand(md: &str, options: Options) -> Cow<'_, str> {
    if !md.contains(":::")
        && !md.contains(":color[")
        && !md.contains(":mark[")
        && !md.contains(":size[")
        && !md.contains("::video[")
        && !md.contains("::place[")
    {
        return Cow::Borrowed(md);
    }
    let boxed = expand_boxes(md, options);
    Cow::Owned(expand_spans(&boxed, options))
}

// ---- 블록 상자 ----

fn box_kind(line: &str) -> Option<&'static str> {
    let rest = line.strip_prefix(":::")?.trim_matches([' ', '\t', '\r']);
    BOX_KINDS.iter().copied().find(|k| *k == rest)
}

fn is_box_close(line: &str) -> bool {
    line.strip_prefix(":::")
        .is_some_and(|r| r.trim_matches([' ', '\t', '\r']).is_empty())
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// 각 줄이 울타리 코드 블록(``` ~~~) 안에 있는가.
fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    let mut fence: Option<(u8, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let t = line.trim_start_matches(' ');
        let run = |c: u8| t.bytes().take_while(|b| *b == c).count();
        match fence {
            Some((c, n)) => {
                out[i] = true;
                if indent < 4 && run(c) >= n && t[run(c)..].trim().is_empty() {
                    fence = None;
                }
            }
            None if indent < 4 => {
                for c in *b"`~" {
                    if run(c) >= 3 {
                        fence = Some((c, run(c)));
                        break;
                    }
                }
            }
            None => {}
        }
    }
    out
}

fn expand_boxes(md: &str, options: Options) -> String {
    let lines: Vec<&str> = md.split('\n').collect();
    let fenced = fenced_lines(&lines);
    let mut out = Vec::with_capacity(lines.len() + 8);
    let doc = Lines {
        lines: &lines,
        fenced: &fenced,
        options,
    };
    expand_range(&doc, 0, lines.len(), &mut out);
    out.join("\n")
}

/// 상자를 펼칠 때 함께 넘기는 글 전체 정보.
struct Lines<'a> {
    lines: &'a [&'a str],
    fenced: &'a [bool],
    options: Options,
}

/// 여는 줄 앞은 빈 줄, 글(또는 상자)의 처음, 또는 다른 상자 줄이어야 한다.
fn opens_at(lines: &[&str], fenced: &[bool], start: usize, l: usize) -> Option<&'static str> {
    if fenced[l] {
        return None;
    }
    let kind = box_kind(lines[l])?;
    if l == start || is_blank(lines[l - 1]) {
        return Some(kind);
    }
    let prev = lines[l - 1];
    (box_kind(prev).is_some() || is_box_close(prev)).then_some(kind)
}

fn expand_range(doc: &Lines<'_>, start: usize, end: usize, out: &mut Vec<String>) {
    let (lines, fenced) = (doc.lines, doc.fenced);
    let mut i = start;
    while i < end {
        if let Some(kind) = opens_at(lines, fenced, start, i)
            && let Some(close) = find_close(lines, fenced, i, end)
        {
            if kind == "details" {
                let (summary, body) = details_summary(doc, i + 1, close);
                out.push("<details class=\"fold\">".to_owned());
                out.push(format!("<summary>{summary}</summary>"));
                out.push(String::new());
                expand_range(doc, body, close, out);
                out.push(String::new());
                out.push("</details>".to_owned());
            } else {
                out.push(format!("<div class=\"{}\">", box_class(kind)));
                out.push(String::new());
                expand_range(doc, i + 1, close, out);
                out.push(String::new());
                out.push("</div>".to_owned());
            }
            out.push(String::new());
            i = close + 1;
            continue;
        }
        if let Some(html) = leaf_at(lines, fenced, start, end, i) {
            out.push(html);
            i += 1;
            continue;
        }
        out.push(lines[i].to_owned());
        i += 1;
    }
}

/// 접기 상자에 요약이 없을 때 보일 글자.
pub const DETAILS_FALLBACK: &str = "자세히";

/// 접기 상자(`:::details`)의 요약 = 상자 안 첫 문단 (ADR-0041). 글자 꾸밈까지 그린 HTML 한 줄과
/// 내용이 시작하는 줄을 돌려준다. 첫 블록이 문단이 아니면(목록·제목·코드·상자) 요약은
/// [`DETAILS_FALLBACK`] 이고 내용은 그 블록부터다. 에디터(`editor/src/markdown.ts`)와 같은 규칙.
fn details_summary(doc: &Lines<'_>, from: usize, close: usize) -> (String, usize) {
    let (lines, fenced) = (doc.lines, doc.fenced);
    let Some(first) = (from..close).find(|&l| !is_blank(lines[l])) else {
        return (DETAILS_FALLBACK.to_owned(), close);
    };
    let fallback = (DETAILS_FALLBACK.to_owned(), first);
    if fenced[first] || box_kind(lines[first]).is_some() || parse_leaf(lines[first]).is_some() {
        return fallback;
    }
    let last = (first..close)
        .find(|&l| is_blank(lines[l]) || fenced[l] || box_kind(lines[l]).is_some())
        .unwrap_or(close);
    let chunk = lines[first..last].join("\n");
    let chunk = expand_spans(&chunk, doc.options);
    // 한 문단이어야 한다. `# 제목`·`- 목록`·`> 인용` 이나 문단 뒤에 바로 붙은 다른 블록은 요약이 아니다.
    let events: Vec<Event<'_>> = Parser::new_ext(&chunk, doc.options).collect();
    let single_paragraph = matches!(events.first(), Some(Event::Start(Tag::Paragraph)))
        && events
            .iter()
            .filter(|e| matches!(e, Event::Start(Tag::Paragraph)))
            .count()
            == 1
        && matches!(events.last(), Some(Event::End(TagEnd::Paragraph)));
    if !single_paragraph {
        return fallback;
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    let inner = html
        .trim()
        .trim_start_matches("<p>")
        .trim_end_matches("</p>")
        .replace('\n', " ");
    (inner, last)
}

// ---- 한 줄 블록: `::video[주소]`, `::place[이름]{주소}` (ADR-0036) ----

/// 넣을 동영상 재생기의 `allow` 값. 정화 단계는 이 값만 남긴다.
pub const VIDEO_ALLOW: &str =
    "accelerometer; encrypted-media; gyroscope; picture-in-picture; fullscreen";

/// 한 줄 블록. 앞은 빈 줄·처음·상자 줄, 뒤는 빈 줄·끝·상자 닫는 줄이어야 한다(에디터와 같은 조건).
fn leaf_at(lines: &[&str], fenced: &[bool], start: usize, end: usize, i: usize) -> Option<String> {
    if fenced[i] {
        return None;
    }
    let prev_ok = i == start || {
        let p = lines[i - 1];
        is_blank(p) || box_kind(p).is_some() || is_box_close(p)
    };
    let next_ok = i + 1 >= end || is_blank(lines[i + 1]) || is_box_close(lines[i + 1]);
    if !prev_ok || !next_ok {
        return None;
    }
    let (kind, label, value) = parse_leaf(lines[i])?;
    match kind {
        // 정화 단계는 iframe 을 받지 않는다. 링크로 두었다가 정화 뒤 [`embed_videos`] 가 재생기로 바꾼다.
        "video" if value.is_none() => video_embed(label)
            .map(|src| format!("<div class=\"video\"><a href=\"{src}\">동영상 보기</a></div>")),
        "place" if !label.trim().is_empty() => {
            let name = label.trim();
            let addr = value.map(str::trim).unwrap_or("");
            let q = url_encode(if addr.is_empty() { name } else { addr });
            Some(format!(
                "<div class=\"place\"><strong class=\"place-name\">{}</strong><span class=\"place-addr\">{}</span>\
                 <span class=\"place-links\"><a href=\"https://map.naver.com/p/search/{q}\">네이버 지도</a> \
                 <a href=\"https://map.kakao.com/?q={q}\">카카오맵</a></span></div>",
                escape(name),
                escape(addr)
            ))
        }
        _ => None,
    }
}

/// `::이름[글자]{값}` 한 줄을 자른다. 글자에는 `]`, 값에는 `}` 가 없어야 한다.
fn parse_leaf(line: &str) -> Option<(&'static str, &str, Option<&str>)> {
    let line = line.trim_end_matches([' ', '\t', '\r']);
    let (kind, rest) = if let Some(r) = line.strip_prefix("::video[") {
        ("video", r)
    } else {
        ("place", line.strip_prefix("::place[")?)
    };
    let (label, after) = rest.split_once(']')?;
    if after.is_empty() {
        return Some((kind, label, None));
    }
    let value = after.strip_prefix('{')?.strip_suffix('}')?;
    (!value.contains('}')).then_some((kind, label, Some(value)))
}

fn yt(id: &str) -> Option<String> {
    (id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| format!("https://www.youtube-nocookie.com/embed/{id}"))
}

/// 유튜브·비메오 주소 → 넣을 수 있는 재생기 주소. 에디터 `videoEmbed` 와 같은 규칙이다.
pub fn video_embed(url: &str) -> Option<String> {
    if url.chars().any(char::is_whitespace) {
        return None;
    }
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (path, query) = match rest.find(['?', '#']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (host, p) = path.split_once('/')?;
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .unwrap_or(host);
    match host {
        "youtu.be" => yt(p),
        "youtube.com" if p == "watch" => {
            let q = query.strip_prefix('?')?;
            let q = q.split('#').next().unwrap_or("");
            q.split('&')
                .find_map(|kv| kv.strip_prefix("v="))
                .and_then(yt)
        }
        "youtube.com" => ["shorts/", "embed/", "live/"]
            .iter()
            .find_map(|pre| p.strip_prefix(pre))
            .and_then(yt),
        "vimeo.com" => (!p.is_empty() && p.len() <= 12 && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| format!("https://player.vimeo.com/video/{p}")),
        _ => None,
    }
}

/// 정화를 마친 HTML 에서 동영상 상자의 링크를 재생기로 바꾼다.
/// 주소가 [`video_embed`] 가 만드는 모양일 때만 바꾸고, 재생기 HTML 은 여기서만 만든다.
pub fn embed_videos(html: &str) -> String {
    const OPEN: &str = "<div class=\"video\"><a href=\"";
    const CLOSE: &str = "</a></div>";
    if !html.contains(OPEN) {
        return html.to_owned();
    }
    let mut out = String::with_capacity(html.len() + 256);
    let mut rest = html;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let parsed = after.split_once('"').and_then(|(src, tail)| {
            let end = tail.find(CLOSE)?;
            // 링크 안에 다른 태그가 끼어 있으면 우리가 만든 상자가 아니다.
            (is_embed_src(src) && !tail[..end].contains('<'))
                .then(|| (src, tail.len() - end - CLOSE.len()))
        });
        match parsed {
            Some((src, remain)) => {
                out.push_str(&format!(
                    "<div class=\"video\"><iframe src=\"{src}\" title=\"동영상\" loading=\"lazy\" \
                     allow=\"{VIDEO_ALLOW}\" allowfullscreen referrerpolicy=\"strict-origin-when-cross-origin\"></iframe></div>"
                ));
                rest = &rest[rest.len() - remain..];
            }
            None => {
                out.push_str(OPEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 재생기 주소로 쓸 수 있는가. [`video_embed`] 가 만드는 모양만.
pub fn is_embed_src(src: &str) -> bool {
    if let Some(id) = src.strip_prefix("https://www.youtube-nocookie.com/embed/") {
        return yt(id).is_some();
    }
    src.strip_prefix("https://player.vimeo.com/video/")
        .is_some_and(|n| !n.is_empty() && n.len() <= 12 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn find_close(lines: &[&str], fenced: &[bool], open: usize, end: usize) -> Option<usize> {
    let mut depth = 1;
    for l in open + 1..end {
        if fenced[l] {
            continue;
        }
        if opens_at(lines, fenced, open + 1, l).is_some() {
            depth += 1;
        } else if is_box_close(lines[l]) {
            depth -= 1;
            if depth == 0 {
                return Some(l);
            }
        }
    }
    None
}

// ---- 글자 꾸밈 ----

/// 코드·HTML 처럼 문법을 바꾸면 안 되는 범위.
fn protected(md: &str, options: Options) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for (event, range) in Parser::new_ext(md, options).into_offset_iter() {
        match event {
            Event::Code(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::Start(Tag::CodeBlock(_))
            | Event::Start(Tag::HtmlBlock) => out.push(range),
            _ => {}
        }
    }
    out
}

fn expand_spans(md: &str, options: Options) -> String {
    if !SPANS
        .iter()
        .any(|(name, _, _)| md.contains(&format!(":{name}[")))
    {
        return md.to_owned();
    }
    let guard = protected(md, options);
    let inside = |p: usize| guard.iter().find(|r| r.contains(&p)).map(|r| r.end);
    let bytes = md.as_bytes();
    // (위치, 바꿀 길이, 새 글자)
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = inside(i) {
            i = end;
            continue;
        }
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] != b':' {
            i += 1;
            continue;
        }
        let found = SPANS.iter().find_map(|(name, prefix, values)| {
            let open = format!(":{name}[");
            if !md[i..].starts_with(&open) {
                return None;
            }
            let label_end = label_end(md, i + open.len() - 1, &inside)?;
            let rest = &md[label_end + 1..];
            let value = rest.strip_prefix('{')?.split_once('}')?.0;
            values.contains(&value).then(|| {
                (
                    open.len(),
                    format!("<span class=\"{prefix}-{value}\">"),
                    label_end,
                    value.len() + 3,
                )
            })
        });
        match found {
            Some((open_len, html, close_at, close_len)) => {
                edits.push((i, open_len, html));
                edits.push((close_at, close_len, "</span>".to_owned()));
                i += open_len;
            }
            None => i += 1,
        }
    }
    if edits.is_empty() {
        return md.to_owned();
    }
    edits.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(md.len() + edits.len() * 16);
    let mut at = 0;
    for (pos, len, text) in edits {
        out.push_str(&md[at..pos]);
        out.push_str(&text);
        at = pos + len;
    }
    out.push_str(&md[at..]);
    out
}

/// `[` 에 짝이 맞는 `]` 의 위치. 줄을 넘지 않는다(에디터와 같은 규칙).
fn label_end(md: &str, open: usize, inside: &dyn Fn(usize) -> Option<usize>) -> Option<usize> {
    let bytes = md.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if i > open
            && let Some(end) = inside(i)
        {
            i = end;
            continue;
        }
        match bytes[i] {
            b'\\' => i += 1,
            b'\n' => return None,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(md: &str) -> String {
        expand(md, Options::empty()).into_owned()
    }

    #[test]
    fn details_first_paragraph_is_summary() {
        let r = |md: &str| crate::markdown::render(md).into_string();
        // 기본: 첫 문단 → 요약(인라인 서식 유지), 나머지 → 내용
        let s = r(
            ":::details\n환불은 **언제** 되나요?\n\n결제 후 7일 안에 됩니다.\n\n- 카드\n- 계좌\n:::",
        );
        assert!(
            s.contains(
                "<details class=\"fold\">\n<summary>환불은 <strong>언제</strong> 되나요?</summary>"
            ),
            "{s}"
        );
        assert!(s.contains("<p>결제 후 7일 안에 됩니다.</p>"), "{s}");
        assert!(s.contains("<li>계좌</li>"), "{s}");
        assert!(s.trim_end().ends_with("</details>"), "{s}");
        assert!(
            !s.contains("<p>환불은"),
            "요약은 내용에 다시 나오지 않는다: {s}"
        );
        // 여러 줄 문단도 한 줄 요약으로, 글자 꾸밈도 그린다
        let s = r(":::details\n배송은\n:color[며칠]{red} 걸려요?\n\n이틀\n:::");
        assert!(
            s.contains("<summary>배송은 <span class=\"tc-red\">며칠</span> 걸려요?</summary>"),
            "{s}"
        );
        // 첫 블록이 문단이 아니면 「자세히」 + 그 블록부터 내용
        for md in [
            ":::details\n- 목록\n:::",
            ":::details\n## 제목\n:::",
            ":::details\n```\n코드\n```\n:::",
            ":::details\n:::info\n안\n:::\n:::",
            ":::details\n:::",
        ] {
            let s = r(md);
            assert!(s.contains("<summary>자세히</summary>"), "{md} -> {s}");
        }
        assert!(r(":::details\n- 목록\n:::").contains("<li>목록</li>"));
        assert!(r(":::details\n:::info\n안\n:::\n:::").contains("callout-info"));
        // 접기 안에 접기 (FAQ 묶음)
        let s = r(
            ":::details\n묶음\n\n:::details\n질문 1\n\n답 1\n:::\n\n:::details\n질문 2\n\n답 2\n:::\n:::",
        );
        assert_eq!(s.matches("<details class=\"fold\">").count(), 3, "{s}");
        assert!(s.contains("<summary>질문 2</summary>"), "{s}");
        // 닫는 줄이 없으면 글자 그대로
        assert!(r(":::details\n질문").contains(":::details"));
        // 사용자가 쓴 details 의 속성·다른 클래스는 지운다
        let s = r(
            "<details open class=\"fold evil\" ontoggle=\"x()\"><summary onclick=\"y()\">a</summary>b</details>",
        );
        assert!(
            !s.contains("open") && !s.contains("evil") && !s.contains("on"),
            "{s}"
        );
        assert!(s.contains("<details class=\"fold\">"), "{s}");
    }

    #[test]
    fn boxes_become_html_blocks() {
        let s = ex(":::info\n알림\n:::");
        assert_eq!(
            s,
            "<div class=\"callout callout-info\">\n\n알림\n\n</div>\n"
        );
        let s = ex(":::tip\n바깥\n\n:::center\n가운데\n:::\n:::");
        assert!(s.contains("<div class=\"callout callout-tip\">"), "{s}");
        assert!(s.contains("<div class=\"align-center\">"), "{s}");
        assert_eq!(s.matches("</div>").count(), 2, "{s}");
    }

    #[test]
    fn video_urls_follow_editor_rules() {
        let yt = Some("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ".to_owned());
        for ok in [
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?feature=x&v=dQw4w9WgXcQ#t=1",
            "https://youtube.com/shorts/dQw4w9WgXcQ?si=1",
        ] {
            assert_eq!(video_embed(ok), yt, "{ok}");
        }
        assert_eq!(
            video_embed("https://vimeo.com/76979871").as_deref(),
            Some("https://player.vimeo.com/video/76979871")
        );
        for bad in [
            "https://youtu.be/short",
            "https://evil.example/youtube.com/watch?v=dQw4w9WgXcQ",
            "javascript:alert(1)",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ\"onload=x",
        ] {
            assert_eq!(video_embed(bad), None, "{bad}");
        }
    }

    #[test]
    fn video_and_place_lines() {
        let s = ex(
            "위\n\n::video[https://youtu.be/dQw4w9WgXcQ]\n\n::place[안목 <해변>]{강원 강릉시 창해로}",
        );
        assert!(
            s.contains("<div class=\"video\"><a href=\"https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ\">"),
            "{s}"
        );
        assert!(s.contains("안목 &lt;해변&gt;"), "{s}");
        assert!(
            s.contains("https://map.naver.com/p/search/%EA%B0%95%EC%9B%90%20"),
            "{s}"
        );
        // 문단에 붙은 줄, 모르는 주소, 값이 붙은 동영상은 글자 그대로.
        for lit in [
            "글\n::video[https://youtu.be/dQw4w9WgXcQ]",
            "::video[https://example.com/v/1]",
            "::video[https://youtu.be/dQw4w9WgXcQ]{x}",
            "::place[ ]",
        ] {
            assert_eq!(ex(lit), lit);
        }
        let boxed = ex(":::center\n::video[https://youtu.be/dQw4w9WgXcQ]\n:::");
        assert!(boxed.contains("<div class=\"video\">"), "{boxed}");
    }

    #[test]
    fn only_checked_video_links_become_players() {
        let s = crate::markdown::render("::video[https://youtu.be/dQw4w9WgXcQ]").into_string();
        assert!(
            s.contains("<iframe src=\"https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ\""),
            "{s}"
        );
        // 사용자가 같은 모양을 직접 써도 주소가 재생기 모양이 아니면 링크로 남는다.
        let s = crate::markdown::render(
            "<div class=\"video\"><a href=\"https://evil.example/x\">동영상 보기</a></div>",
        )
        .into_string();
        assert!(!s.contains("<iframe"), "{s}");
        let s = crate::markdown::render(
            "<iframe src=\"https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ\"></iframe>",
        )
        .into_string();
        assert!(!s.contains("<iframe"), "{s}");
    }

    #[test]
    fn boxes_need_close_blank_line_and_no_fence() {
        assert_eq!(ex(":::info\n닫지 않음"), ":::info\n닫지 않음");
        assert_eq!(ex("문단\n:::info\n가\n:::"), "문단\n:::info\n가\n:::");
        assert_eq!(
            ex("```\n:::info\n가\n:::\n```"),
            "```\n:::info\n가\n:::\n```"
        );
        assert_eq!(ex(":::evil\n가\n:::"), ":::evil\n가\n:::");
        assert_eq!(ex("  :::info\n가\n:::"), "  :::info\n가\n:::");
    }

    #[test]
    fn spans_become_classes_and_unknown_values_stay_text() {
        let s = ex("이건 :color[빨간 **굵게**]{red} 와 :mark[노랑]{yellow}");
        assert_eq!(
            s,
            "이건 <span class=\"tc-red\">빨간 **굵게**</span> 와 <span class=\"hl-yellow\">노랑</span>"
        );
        assert_eq!(ex(":color[가]{magenta}"), ":color[가]{magenta}");
        assert_eq!(ex("`:color[가]{red}`"), "`:color[가]{red}`");
        assert_eq!(ex("\\:color[가]{red}"), "\\:color[가]{red}");
        assert_eq!(ex(":color[가\n나]{red}"), ":color[가\n나]{red}");
        let nested = ex(":color[a :size[b]{large}]{red}");
        assert_eq!(
            nested,
            "<span class=\"tc-red\">a <span class=\"fs-large\">b</span></span>"
        );
    }
}
