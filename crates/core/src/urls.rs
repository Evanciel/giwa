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
    Board { board: &'a str },
    Post { board: &'a str, post: &'a str },
    Page { page: &'a str },
    Sitemap,
    Rss,
}

fn enc(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

/// 사이트 안 경로 (`/` 로 시작).
pub fn path(target: &Target<'_>) -> String {
    match target {
        Target::Home => "/".to_owned(),
        Target::Board { board } => format!("/b/{}", enc(board)),
        Target::Post { board, post } => format!("/b/{}/{}", enc(board), enc(post)),
        Target::Page { page } => format!("/p/{}", enc(page)),
        Target::Sitemap => "/sitemap.xml".to_owned(),
        Target::Rss => "/rss.xml".to_owned(),
    }
}

/// `site_url` 기준 절대 URL. `Host` 헤더는 쓰지 않는다.
pub fn absolute(site_url: &str, target: &Target<'_>) -> String {
    format!("{}{}", site_url.trim_end_matches('/'), path(target))
}

/// 첨부 파일 내려받기 주소.
pub fn file(id: &str) -> String {
    format!("/files/{id}")
}

/// 미디어 파일 주소. 보관소 경로는 ASCII 규칙(`blobs::valid_path`)을 따르므로 인코딩하지 않는다.
pub fn media(blob_path: &str) -> String {
    format!("/media/{blob_path}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn absolute_uses_site_url() {
        assert_eq!(
            absolute("https://blog.example.com/", &Target::Rss),
            "https://blog.example.com/rss.xml"
        );
    }
}
