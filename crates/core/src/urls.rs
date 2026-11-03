//! 공개 URL 은 여기서만 만든다 (ADR-0017). 템플릿·사이트맵·RSS·CLI 출력이 모두 이 함수를 쓴다.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

/// 경로 조각에서 그대로 둘 수 없는 문자. 한글 등 비 ASCII 는 UTF-8 퍼센트 인코딩된다.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'\\')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target<'a> {
    Home,
    Board {
        board: &'a str,
    },
    Post {
        board: &'a str,
        post: &'a str,
    },
    Page {
        page: &'a str,
    },
    /// 게시판의 한 분류 (`/b/{board}?category={slug}`)
    Category {
        board: &'a str,
        category: &'a str,
    },
    Tags,
    Tag {
        tag: &'a str,
    },
    Sitemap,
    Rss,
    /// 사이트 검색 화면. 검색어는 [`with_query`] 로 붙인다.
    Search,
}

/// 쿼리 값에서는 `&`·`=`·`+` 도 인코딩한다.
const QUERY: &AsciiSet = &SEGMENT.add(b'&').add(b'=').add(b'+');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

fn enc_query(s: &str) -> String {
    utf8_percent_encode(s, QUERY).to_string()
}

/// 사이트 안 경로 (`/` 로 시작).
pub fn path(target: &Target<'_>) -> String {
    match target {
        Target::Home => "/".to_owned(),
        Target::Board { board } => format!("/b/{}", enc(board)),
        Target::Post { board, post } => format!("/b/{}/{}", enc(board), enc(post)),
        Target::Page { page } => format!("/p/{}", enc(page)),
        Target::Category { board, category } => {
            format!("/b/{}?category={}", enc(board), enc_query(category))
        }
        Target::Tags => "/tags".to_owned(),
        Target::Tag { tag } => format!("/tags/{}", enc(tag)),
        Target::Sitemap => "/sitemap.xml".to_owned(),
        Target::Rss => "/rss.xml".to_owned(),
        Target::Search => "/search".to_owned(),
    }
}

/// 경로에 쿼리를 붙인다. 빈 값은 뺀다. 경로에 이미 `?` 가 있으면 `&` 로 잇는다.
pub fn with_query(path: &str, params: &[(&str, &str)]) -> String {
    let mut out = path.to_owned();
    let mut sep = if path.contains('?') { '&' } else { '?' };
    for (k, v) in params {
        if v.is_empty() {
            continue;
        }
        out.push(sep);
        out.push_str(k);
        out.push('=');
        out.push_str(&enc_query(v));
        sep = '&';
    }
    out
}

/// `site_url` 기준 절대 URL. `Host` 헤더는 쓰지 않는다.
pub fn absolute(site_url: &str, target: &Target<'_>) -> String {
    format!("{}{}", site_url.trim_end_matches('/'), path(target))
}

/// 첨부 파일 내려받기 주소.
pub fn file(id: &str) -> String {
    format!("/files/{id}")
}

/// 올린 서체 파일 주소. 보관소 경로(`fonts/{slug}/{hash}.{ext}`)의 마지막 조각을 쓴다.
pub fn font_file(slug: &str, path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or_default();
    format!("/fonts/{slug}/{name}")
}

/// 올린 서체의 라이선스 전문 주소.
pub fn font_license(slug: &str) -> String {
    format!("/fonts/{slug}/LICENSE.txt")
}

/// 미디어 파일 주소. 보관소 경로는 ASCII 규칙(`blobs::valid_path`)을 따르므로 인코딩하지 않는다.
pub fn media(blob_path: &str) -> String {
    format!("/media/{blob_path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_encoded_and_empty_ones_dropped() {
        assert_eq!(
            with_query(
                &path(&Target::Search),
                &[("q", "제주 a&b"), ("in", ""), ("sort", "new")]
            ),
            "/search?q=%EC%A0%9C%EC%A3%BC%20a%26b&sort=new"
        );
        assert_eq!(
            with_query("/b/x?category=y", &[("q", "z")]),
            "/b/x?category=y&q=z"
        );
    }

    #[test]
    fn hangul_is_percent_encoded() {
        assert_eq!(
            path(&Target::Post {
                board: "blog",
                post: "첫-글"
            }),
            "/b/blog/%EC%B2%AB-%EA%B8%80"
        );
        assert_eq!(path(&Target::Board { board: "a/b" }), "/b/a%2Fb");
        assert_eq!(
            path(&Target::Category {
                board: "blog",
                category: "a&b=c"
            }),
            "/b/blog?category=a%26b%3Dc"
        );
        assert_eq!(
            path(&Target::Tag { tag: "제주" }),
            "/tags/%EC%A0%9C%EC%A3%BC"
        );
    }

    #[test]
    fn absolute_uses_site_url() {
        assert_eq!(
            absolute("https://blog.example.com/", &Target::Rss),
            "https://blog.example.com/rss.xml"
        );
    }
}
