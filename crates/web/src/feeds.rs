//! 사이트맵(sitemaps.org 0.9), RSS 2.0, robots.txt.
//!
//! 공개 화면과 같은 비로그인 Actor·공개 조회 서비스만 쓰므로 숨김·삭제·초안·비공개 게시판 글은
//! 화면에서 사라지는 순간 여기서도 사라진다. 모든 URL 은 `urls::absolute` 로만 만든다.

use std::fmt::Write as _;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use maru_core::model::{Board, PageReq, Post};
use maru_core::services::{anonymous_actor, boards, pages, posts, settings};
use maru_core::urls::{self, Target};
use maru_core::{Result, clock, markdown};

use crate::{AppState, error};

/// 사이트맵 한 파일의 URL 상한 (sitemaps.org).
const SITEMAP_MAX: usize = 50_000;
const RSS_ITEMS: u32 = 20;

pub async fn sitemap(State(state): State<AppState>) -> Response {
    match sitemap_xml(&state).await {
        Ok(xml) => xml_response(xml, "application/xml; charset=utf-8"),
        Err(e) => error::page(&state, &e).await,
    }
}

pub async fn rss(State(state): State<AppState>) -> Response {
    match rss_xml(&state).await {
        Ok(xml) => xml_response(xml, "application/rss+xml; charset=utf-8"),
        Err(e) => error::page(&state, &e).await,
    }
}

pub async fn robots(State(state): State<AppState>) -> Response {
    let sitemap = urls::absolute(&state.ctx.site_url, &Target::Sitemap);
    let body = format!("User-agent: *\nAllow: /\nDisallow: /api/\n\nSitemap: {sitemap}\n");
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}

fn xml_response(xml: String, mime: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=300"),
        ],
        xml,
    )
        .into_response()
}

/// 공개 글 전부 (게시판 슬러그와 함께). 화면 목록과 같은 서비스를 페이지 단위로 돈다.
/// 비밀글은 제목만 보이는 글이라 피드·사이트맵에는 아예 넣지 않는다.
async fn all_public_posts(state: &AppState, max: usize) -> Result<Vec<(String, Post)>> {
    let actor = anonymous_actor(&state.ctx).await?;
    let all_boards = state.ctx.store.list_boards().await?;
    let filter = posts::PublicFilter {
        exclude_secret: true,
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut page = 1;
    loop {
        let list = posts::public_posts_where(
            &state.ctx,
            &actor,
            None,
            &filter,
            PageReq::new(page, PageReq::MAX_PER_PAGE),
        )
        .await?;
        let done = list.items.is_empty() || i64::from(page) >= list.total_pages();
        for p in list.items {
            if let Some(b) = all_boards.iter().find(|b| b.id == p.board_id) {
                out.push((b.slug.clone(), p));
            }
            if out.len() >= max {
                return Ok(out);
            }
        }
        if done {
            return Ok(out);
        }
        page += 1;
    }
}

async fn sitemap_xml(state: &AppState) -> Result<String> {
    let site = &state.ctx.site_url;
    let actor = anonymous_actor(&state.ctx).await?;
    let mut entries: Vec<(String, Option<String>)> =
        vec![(urls::absolute(site, &Target::Home), None)];
    let readable: Vec<Board> = boards::list(&state.ctx, &actor).await?;
    for b in &readable {
        entries.push((
            urls::absolute(site, &Target::Board { board: &b.slug }),
            None,
        ));
    }
    let home_page = settings::site(&state.ctx).await?.home_page;
    for p in pages::public_pages(&state.ctx, &actor)
        .await?
        .into_iter()
        .filter(|p| p.slug != home_page && !p.noindex)
    {
        entries.push((
            urls::absolute(site, &Target::Page { page: &p.slug }),
            Some(p.updated_at),
        ));
    }
    let room = SITEMAP_MAX.saturating_sub(entries.len());
    for (board, p) in all_public_posts(state, room).await? {
        entries.push((
            urls::absolute(
                site,
                &Target::Post {
                    board: &board,
                    post: &p.slug,
                },
            ),
            Some(p.updated_at),
        ));
    }

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for (loc, lastmod) in entries {
        let _ = write!(xml, "  <url><loc>{}</loc>", escape(&loc));
        if let Some(m) = lastmod {
            let _ = write!(xml, "<lastmod>{}</lastmod>", escape(&m));
        }
        xml.push_str("</url>\n");
    }
    xml.push_str("</urlset>\n");
    Ok(xml)
}

async fn rss_xml(state: &AppState) -> Result<String> {
    let site_url = &state.ctx.site_url;
    let s = settings::site(&state.ctx).await?;
    let items = all_public_posts(state, RSS_ITEMS as usize).await?;
    let home = urls::absolute(site_url, &Target::Home);
    let self_url = urls::absolute(site_url, &Target::Rss);
    let last = items
        .first()
        .and_then(|(_, p)| p.published_at.clone())
        .unwrap_or_else(clock::now);

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n<channel>\n",
    );
    let _ = writeln!(xml, "  <title>{}</title>", escape(&s.name));
    let _ = writeln!(xml, "  <link>{}</link>", escape(&home));
    let description = if s.description.is_empty() {
        &s.name
    } else {
        &s.description
    };
    let _ = writeln!(xml, "  <description>{}</description>", escape(description));
    let _ = writeln!(xml, "  <language>{}</language>", escape(&s.lang));
    let _ = writeln!(
        xml,
        "  <lastBuildDate>{}</lastBuildDate>",
        clock::rfc2822(&last)
    );
    let _ = writeln!(
        xml,
        "  <atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>",
        escape(&self_url)
    );
    for (board, p) in &items {
        let link = urls::absolute(
            site_url,
            &Target::Post {
                board,
                post: &p.slug,
            },
        );
        let published = p.published_at.clone().unwrap_or_default();
        xml.push_str("  <item>\n");
        let _ = writeln!(xml, "    <title>{}</title>", escape(&p.title));
        let _ = writeln!(xml, "    <link>{}</link>", escape(&link));
        let _ = writeln!(
            xml,
            "    <guid isPermaLink=\"true\">{}</guid>",
            escape(&link)
        );
        let _ = writeln!(xml, "    <pubDate>{}</pubDate>", clock::rfc2822(&published));
        // 본문 전체를 정화된 HTML 로 싣되 CDATA 없이 이스케이프한다 (PHASE1_PLAN §7).
        let html = markdown::render(&p.body_md).into_string();
        let _ = writeln!(xml, "    <description>{}</description>", escape(&html));
        xml.push_str("  </item>\n");
    }
    xml.push_str("</channel>\n</rss>\n");
    Ok(xml)
}

/// XML 텍스트·속성 이스케이프.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 에서 쓸 수 없는 제어 문자는 버린다.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_handles_markup_and_controls() {
        assert_eq!(
            escape("<a href='x'>&\u{0}"),
            "&lt;a href=&apos;x&apos;&gt;&amp;"
        );
    }
}
