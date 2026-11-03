//! 사이트 검색의 글자 자르기·검색어 해석·본문 토막 (GAP C5, ADR-0050). DB 비의존.
//!
//! - SQLite FTS5 기본 토크나이저는 띄어쓰기로만 자른다. 「제주도에서」 를 한 낱말로 넣으면 「제주」 로 못 찾으므로
//!   한글·한자·가나 덩어리는 여기서 2글자 단위로 잘라 넣는다: 「제주도」 → `제주 주도 도`.
//!   덩어리 끝 글자는 한 글자로도 넣어 1글자 검색어가 끝 글자에도 걸리게 한다.
//! - 검색어도 같은 규칙으로 잘라 **이어진 구(phrase)** 로 찾는다. 사용자 입력은 FTS5 문법으로 직접 넘기지 않고
//!   모든 토막을 큰따옴표로 감싼 질의로 다시 만든다.
//! - 영문·숫자는 낱말 그대로(소문자). 입력은 NFC 로 맞춘다(맥 등에서 오는 자모 분리 입력).

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::error::{Code, Error, Result};
use crate::markdown;

/// 자르는 규칙을 바꾸면 올린다. DB 를 열 때 저장된 값과 다르면 색인을 다시 만든다.
pub const INDEX_VERSION: i64 = 1;
/// 검색어 최대 글자 수.
pub const MAX_QUERY_CHARS: usize = 100;
/// 검색어 낱말(구·빼기 포함) 최대 수.
pub const MAX_TERMS: usize = 10;
/// 결과 본문 토막 글자 수.
pub const SNIPPET_CHARS: usize = 120;

/// 한글·한자·가나처럼 띄어쓰기만으로 낱말을 가를 수 없는 글자.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{AC00}'..='\u{D7A3}'   // 한글 음절
        | '\u{1100}'..='\u{11FF}' // 한글 자모
        | '\u{3130}'..='\u{318F}' // 한글 호환 자모
        | '\u{3040}'..='\u{30FF}' // 히라가나·가타카나
        | '\u{3400}'..='\u{4DBF}' // 한자 확장 A
        | '\u{4E00}'..='\u{9FFF}' // 한자
        | '\u{F900}'..='\u{FAFF}' // 한자 호환
    )
}

fn normalize(s: &str) -> String {
    s.nfc().flat_map(char::to_lowercase).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Cjk,
    Word,
}

/// 글자를 덩어리로 나눈다: 한글 등 덩어리, 영문·숫자 낱말. 그 밖의 글자는 경계다.
fn runs(s: &str) -> Vec<(RunKind, Vec<char>)> {
    let mut out: Vec<(RunKind, Vec<char>)> = Vec::new();
    for c in normalize(s).chars() {
        let kind = if is_cjk(c) {
            RunKind::Cjk
        } else if c.is_alphanumeric() {
            RunKind::Word
        } else {
            out.push((RunKind::Word, Vec::new()));
            continue;
        };
        match out.last_mut() {
            Some((k, chars)) if *k == kind && (!chars.is_empty() || kind == RunKind::Word) => {
                chars.push(c);
                *k = kind;
            }
            _ => out.push((kind, vec![c])),
        }
    }
    out.retain(|(_, chars)| !chars.is_empty());
    out
}

fn bigrams(chars: &[char]) -> Vec<String> {
    chars.windows(2).map(|w| w.iter().collect()).collect()
}

fn index_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (kind, chars) in runs(s) {
        match kind {
            RunKind::Word => out.push(chars.iter().collect()),
            RunKind::Cjk if chars.len() == 1 => out.push(chars[0].to_string()),
            RunKind::Cjk => {
                out.extend(bigrams(&chars));
                // 끝 글자도 한 글자로: 「올레길」 을 「길」 로도 찾게.
                if let Some(last) = chars.last() {
                    out.push(last.to_string());
                }
            }
        }
    }
    out
}

/// 색인에 넣을 글자. 토막을 띄어 쓴 문자열이라 FTS5 `unicode61` 가 그대로 낱말로 받는다.
pub fn index_text(s: &str) -> String {
    index_tokens(s).join(" ")
}

/// 검색할 때의 토막. 마지막 덩어리를 빼면 색인과 똑같이 자르고(구가 덩어리를 넘어도 위치가 맞게),
/// 마지막 덩어리는 끝 글자를 넣지 않는다(「올레길」 로 「올레길을」 도 찾게). 마지막이 한 글자면 앞부분 일치.
fn query_tokens(s: &str) -> (Vec<String>, bool) {
    let runs = runs(s);
    let mut out = Vec::new();
    let mut prefix = false;
    let n = runs.len();
    for (i, (kind, chars)) in runs.into_iter().enumerate() {
        let last = i + 1 == n;
        match kind {
            RunKind::Word => out.push(chars.iter().collect()),
            RunKind::Cjk if chars.len() == 1 => {
                out.push(chars[0].to_string());
                prefix = last;
            }
            RunKind::Cjk => {
                out.extend(bigrams(&chars));
                if !last && let Some(c) = chars.last() {
                    out.push(c.to_string());
                }
            }
        }
    }
    (out, prefix)
}

/// 색인할 글 한 편 (글·페이지 공통).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub title: String,
    pub body: String,
    pub tags: String,
}

impl Document {
    /// 마크다운은 걷어낸 글자로 넣는다(꾸밈 지시자 포함).
    pub fn new(title: &str, body_md: &str, tags: &[String]) -> Self {
        Self {
            title: index_text(title),
            body: index_text(&markdown::plain_text(body_md, usize::MAX)),
            tags: index_text(&tags.join(" ")),
        }
    }
}

/// 어디서 찾나.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    /// 제목·내용·태그
    #[default]
    All,
    Title,
    Body,
    Tag,
}

impl Range {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "" | "all" => Some(Self::All),
            "title" => Some(Self::Title),
            "body" => Some(Self::Body),
            "tag" | "tags" => Some(Self::Tag),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Title => "title",
            Self::Body => "body",
            Self::Tag => "tag",
        }
    }

    fn column(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Title => Some("title"),
            Self::Body => Some("body"),
            Self::Tag => Some("tags"),
        }
    }
}

/// 검색어 한 토막: 낱말 하나 또는 큰따옴표 구.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// 사용자가 적은 글자(정규화). 본문 토막에서 강조할 때 쓴다.
    pub text: String,
    tokens: Vec<String>,
    prefix: bool,
}

impl Term {
    fn new(raw: &str) -> Option<Self> {
        let (tokens, prefix) = query_tokens(raw);
        if tokens.is_empty() {
            return None;
        }
        let text = normalize(raw)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        Some(Self {
            text,
            tokens,
            prefix,
        })
    }

    fn fts(&self, range: Range) -> String {
        // 토막은 글자·숫자만 들어 있어 큰따옴표가 없다.
        let phrase = format!(
            "\"{}\"{}",
            self.tokens.join(" "),
            if self.prefix { "*" } else { "" }
        );
        match range.column() {
            Some(col) => format!("{{{col}}} : {phrase}"),
            None => phrase,
        }
    }
}

/// 해석한 검색어. 묶음끼리는 모두(AND), 묶음 안은 어느 하나(OR), `exclude` 는 빼기.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub groups: Vec<Vec<Term>>,
    pub exclude: Vec<Term>,
}

fn query_error(q: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.search_query")
        .arg("query", q.to_owned())
        .arg("max", MAX_QUERY_CHARS.to_string())
        .with_hint("hint.search_query")
}

/// 띄어 쓴 낱말 / `"구"` / `-빼기` / `OR`(또는 `|`) 로 나눈 낱말.
#[derive(Debug, PartialEq, Eq)]
enum Lex {
    Word { text: String, negate: bool },
    Or,
}

fn lex(q: &str) -> Vec<Lex> {
    let mut out = Vec::new();
    let mut chars = q.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut negate = false;
        if c == '-' {
            chars.next();
            negate = true;
        }
        let mut text = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                text.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                text.push(c);
                chars.next();
            }
        }
        if !negate && (text == "OR" || text == "|") {
            out.push(Lex::Or);
        } else if !text.is_empty() {
            out.push(Lex::Word { text, negate });
        }
    }
    out
}

impl Query {
    /// 사용자 검색어를 해석한다. 빈 검색어, 빼기만 있는 검색어, 한도를 넘는 검색어는 검증 오류.
    pub fn parse(q: &str) -> Result<Self> {
        let q = q.trim();
        if q.is_empty() || q.chars().count() > MAX_QUERY_CHARS {
            return Err(query_error(q));
        }
        let mut groups: Vec<Vec<Term>> = Vec::new();
        let mut exclude = Vec::new();
        let mut join_next = false;
        let mut count = 0usize;
        for lx in lex(q) {
            match lx {
                Lex::Or => join_next = !groups.is_empty(),
                Lex::Word { text, negate } => {
                    let Some(term) = Term::new(&text) else {
                        continue;
                    };
                    count += 1;
                    if count > MAX_TERMS {
                        return Err(query_error(q));
                    }
                    if negate {
                        exclude.push(term);
                    } else if join_next && let Some(last) = groups.last_mut() {
                        last.push(term);
                    } else {
                        groups.push(vec![term]);
                    }
                    join_next = false;
                }
            }
        }
        if groups.is_empty() {
            return Err(query_error(q));
        }
        Ok(Self { groups, exclude })
    }

    /// FTS5 `MATCH` 에 넘길 질의.
    pub fn to_fts(&self, range: Range) -> String {
        let group = |g: &Vec<Term>| {
            let parts: Vec<String> = g.iter().map(|t| t.fts(range)).collect();
            if parts.len() == 1 {
                parts.concat()
            } else {
                format!("({})", parts.join(" OR "))
            }
        };
        let mut out = self
            .groups
            .iter()
            .map(group)
            .collect::<Vec<_>>()
            .join(" AND ");
        for t in &self.exclude {
            out.push_str(" NOT ");
            out.push_str(&t.fts(range));
        }
        out
    }

    /// 강조할 글자들 (빼기는 넣지 않는다).
    pub fn highlights(&self) -> Vec<&str> {
        self.groups
            .iter()
            .flatten()
            .map(|t| t.text.as_str())
            .collect()
    }
}

/// 본문 토막의 한 조각. `hit` 이면 검색어와 맞은 부분이다(화면이 `<mark>` 로 감싼다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Segment {
    pub text: String,
    pub hit: bool,
}

/// 평문에서 검색어 둘레 `max_chars` 글자를 잘라 맞은 부분을 표시한다.
/// 맞은 곳이 없으면(태그로만 걸린 글 등) 앞부분을 준다.
pub fn snippet(plain: &str, query: &Query, max_chars: usize) -> Vec<Segment> {
    let chars: Vec<char> = plain.chars().collect();
    // 글자 수가 바뀌지 않게 한 글자씩 소문자로 맞춘다.
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut hits = vec![false; chars.len()];
    let mut first: Option<usize> = None;
    for needle in query.highlights() {
        let needle: Vec<char> = needle.chars().collect();
        if needle.is_empty() || needle.len() > lower.len() {
            continue;
        }
        for start in 0..=(lower.len() - needle.len()) {
            if lower[start..start + needle.len()] == needle[..] {
                hits[start..start + needle.len()].fill(true);
                first = Some(first.map_or(start, |f| f.min(start)));
            }
        }
    }
    let start = first.map_or(0, |f| f.saturating_sub(max_chars / 3));
    let end = (start + max_chars).min(chars.len());
    let start = end.saturating_sub(max_chars).min(start);
    let mut out: Vec<Segment> = Vec::new();
    if start > 0 {
        out.push(Segment {
            text: "…".into(),
            hit: false,
        });
    }
    for i in start..end {
        match out.last_mut() {
            Some(seg) if seg.hit == hits[i] && !(i == start && start > 0) => {
                seg.text.push(chars[i])
            }
            _ => out.push(Segment {
                text: chars[i].to_string(),
                hit: hits[i],
            }),
        }
    }
    if end < chars.len() {
        match out.last_mut() {
            Some(seg) if !seg.hit => seg.text.push('…'),
            _ => out.push(Segment {
                text: "…".into(),
                hit: false,
            }),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fts(q: &str) -> String {
        Query::parse(q).unwrap().to_fts(Range::All)
    }

    #[test]
    fn hangul_is_cut_into_pairs() {
        assert_eq!(index_text("제주도에서"), "제주 주도 도에 에서 서");
        assert_eq!(index_text("길"), "길");
        assert_eq!(index_text("Rust로 만든 CMS 2.0"), "rust 로 만든 든 cms 2 0");
        assert_eq!(index_text("올레길, 7코스!"), "올레 레길 길 7 코스 스");
    }

    #[test]
    fn decomposed_input_is_normalized() {
        let nfd: String = "제주".nfd().collect();
        assert_ne!(nfd, "제주");
        assert_eq!(index_text(&nfd), "제주 주");
        assert_eq!(fts(&nfd), "\"제주\"");
    }

    #[test]
    fn query_words_become_phrases() {
        assert_eq!(fts("제주"), "\"제주\"");
        assert_eq!(fts("제주도"), "\"제주 주도\"");
        assert_eq!(fts("길"), "\"길\"*");
        assert_eq!(fts("제주 국밥"), "\"제주\" AND \"국밥\"");
        assert_eq!(fts("\"제주 올레\""), "\"제주 주 올레\"");
        assert_eq!(fts("\"제주 길\""), "\"제주 주 길\"*");
        assert_eq!(
            fts("제주 OR 부산 국밥"),
            "(\"제주\" OR \"부산\") AND \"국밥\""
        );
        assert_eq!(fts("제주 | 부산"), "(\"제주\" OR \"부산\")");
        assert_eq!(fts("국밥 -부산"), "\"국밥\" NOT \"부산\"");
        assert_eq!(fts("Rust"), "\"rust\"");
        assert_eq!(
            Query::parse("제주").unwrap().to_fts(Range::Title),
            "{title} : \"제주\""
        );
    }

    #[test]
    fn index_and_query_line_up() {
        // 검색 토막이 색인 토막 안에 이어서 들어 있어야 구 검색이 맞는다.
        let contains = |doc: &str, q: &str| {
            let d = index_tokens(doc);
            let (t, prefix) = query_tokens(q);
            d.windows(t.len()).any(|w| {
                w[..t.len() - 1] == t[..t.len() - 1]
                    && if prefix {
                        w[t.len() - 1].starts_with(&t[t.len() - 1])
                    } else {
                        w[t.len() - 1] == t[t.len() - 1]
                    }
            })
        };
        assert!(contains("제주도에서 걸었다", "제주"));
        assert!(contains("제주도에서 걸었다", "제주도"));
        assert!(contains("올레길을 걸었다", "올레길"));
        assert!(contains("제주 올레길", "제주 올레"));
        assert!(contains("올레길", "길"));
        assert!(contains("길을 걸었다", "길"));
        assert!(!contains("주도권 제주", "제주도"));
    }

    #[test]
    fn hostile_input_cannot_break_the_query() {
        for q in [
            "\"",
            "제주\"",
            "NEAR(a b)",
            "title:제주",
            "{title} : x",
            "a* ^b",
            "(제주",
            "AND OR NOT",
            "'; DROP TABLE posts; --",
        ] {
            let Ok(parsed) = Query::parse(q) else {
                continue;
            };
            let f = parsed.to_fts(Range::All);
            // 큰따옴표 밖에는 연산자·괄호·별표만 있다.
            let outside: String = f.split('"').step_by(2).collect::<Vec<_>>().concat();
            assert!(
                outside.split_whitespace().all(|w| [
                    "AND", "OR", "NOT", "(", ")", "*", "*)", "{title}", ":"
                ]
                .contains(&w)
                    || w.chars().all(|c| c == '(' || c == ')' || c == '*')),
                "{q} → {f}"
            );
        }
    }

    #[test]
    fn bad_queries_are_rejected() {
        assert!(Query::parse("").is_err());
        assert!(Query::parse("   ").is_err());
        assert!(Query::parse("!!! ???").is_err());
        assert!(Query::parse("-부산").is_err());
        assert!(Query::parse(&"가".repeat(MAX_QUERY_CHARS + 1)).is_err());
        assert!(Query::parse("가 나 다 라 마 바 사 아 자 차 카").is_err());
        assert!(Query::parse("OR 제주").is_ok());
    }

    #[test]
    fn snippet_marks_hits() {
        let q = Query::parse("제주 Rust").unwrap();
        let s = snippet("나는 rust 로 제주 여행기를 썼다.", &q, 120);
        assert_eq!(
            s,
            vec![
                Segment {
                    text: "나는 ".into(),
                    hit: false
                },
                Segment {
                    text: "rust".into(),
                    hit: true
                },
                Segment {
                    text: " 로 ".into(),
                    hit: false
                },
                Segment {
                    text: "제주".into(),
                    hit: true
                },
                Segment {
                    text: " 여행기를 썼다.".into(),
                    hit: false
                },
            ]
        );
    }

    #[test]
    fn snippet_centres_on_first_hit() {
        let text = format!("{}제주{}", "가".repeat(200), "나".repeat(200));
        let q = Query::parse("제주").unwrap();
        let s = snippet(&text, &q, 30);
        assert_eq!(s.first().unwrap().text, "…");
        assert!(s.iter().any(|g| g.hit && g.text == "제주"));
        assert!(s.last().unwrap().text.ends_with('…'));
        let shown: usize = s.iter().map(|g| g.text.chars().count()).sum();
        assert_eq!(shown, 30 + 2);
        // 맞은 곳이 없으면 앞부분.
        let s = snippet("태그로만 걸린 글", &q, 120);
        assert_eq!(
            s,
            vec![Segment {
                text: "태그로만 걸린 글".into(),
                hit: false
            }]
        );
    }
}
