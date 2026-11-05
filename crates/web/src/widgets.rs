//! 위젯 화면 값 (ADR-0059). core 가 돌려준 배치를 종류마다 템플릿 값으로 바꾼다.
//!
//! 공개 화면은 손님 기준이다. 위젯 하나가 실패하거나(게시판이 사라짐 등) 손님이 못 읽는
//! 게시판을 가리키면 그 위젯만 빠지고 화면은 그대로 뜬다.

use maru_core::model::{PageReq, SiteSettings, Widget};
use maru_core::services::posts::PublicFilter;
use maru_core::services::widgets::{
    self, AccountBoxConfig, GalleryGridConfig, ImageBannerConfig, LatestPostsConfig,
    MarkdownBlockConfig, MenuConfig, NoticeBarConfig, PopularPostsConfig, PopupConfig,
    PostTabsConfig, Screen, SearchConfig,
};
use maru_core::services::{anonymous_actor, boards, media, posts, settings};
use maru_core::urls::{self, Target};
use maru_core::{Actor, Result, markdown};
use serde_json::{Map, Value, json};

use crate::AppState;
use crate::public::{menu_value, summaries_any_board};

/// 화면에 붙일 위젯을 영역별로: `{ "sidebar": [..], "main": [..] }`. 없으면 빈 객체.
pub(crate) async fn regions(state: &AppState, screen: Screen<'_>) -> Value {
    let list = match widgets::for_screen(&state.ctx, screen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = ?e, "위젯 목록을 못 읽어 위젯 없이 그립니다");
            return json!({});
        }
    };
    if list.is_empty() {
        return json!({});
    }
    let (actor, site) = match (
        anonymous_actor(&state.ctx).await,
        settings::site(&state.ctx).await,
    ) {
        (Ok(a), Ok(s)) => (a, s),
        _ => return json!({}),
    };
    let mut out = Map::new();
    for w in &list {
        match view(state, &actor, &site, w).await {
            Ok(Some(v)) => {
                if let Some(arr) = out
                    .entry(w.region.clone())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                {
                    arr.push(v);
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(widget = %w.id, kind = %w.kind, error = ?e, "위젯을 빼고 그립니다")
            }
        }
    }
    Value::Object(out)
}

/// 모든 게시판 인기 모아보기 주소 (GAP Q6)
fn recent_popular_url() -> String {
    urls::with_query(&urls::path(&Target::Recent), &[("sort", "popular")])
}

fn cfg<T: serde::de::DeserializeOwned>(w: &Widget) -> Result<T> {
    serde_json::from_value(w.config.clone()).map_err(maru_core::Error::internal)
}

/// 위젯 하나의 템플릿 값. 그릴 것이 없으면 None.
async fn view(
    state: &AppState,
    actor: &Actor,
    site: &SiteSettings,
    w: &Widget,
) -> Result<Option<Value>> {
    let ctx = &state.ctx;
    let mut more_url = Value::Null;
    // 모듈 위젯만 채운다 (ADR-0066).
    let mut template = Value::Null;
    let mut module_label = None;
    let data = match w.kind.as_str() {
        "latest_posts" => {
            let c: LatestPostsConfig = cfg(w)?;
            let Some(board) = readable(state, actor, &c.board).await? else {
                return Ok(None);
            };
            // 게시판을 고르지 않았으면 모든 게시판 모아보기로 (GAP Q6)
            more_url = json!(match &board {
                Some(b) => urls::path(&Target::Board { board: &b.slug }),
                None => urls::path(&Target::Recent),
            });
            let filter = PublicFilter {
                exclude_secret: true,
                ..PublicFilter::default()
            };
            let list = posts::public_posts_where(
                ctx,
                actor,
                board.as_ref(),
                &filter,
                PageReq::new(1, c.limit * c.pages),
            )
            .await?;
            let items = summaries_any_board(state, &list.items, site).await?;
            json!({
                "posts": items,
                "view": c.view,
                "show_date": c.show_date,
                "page_size": c.limit,
                "pages": c.pages,
                "auto": c.auto,
            })
        }
        "popular_posts" => {
            let c: PopularPostsConfig = cfg(w)?;
            let Some(board) = readable(state, actor, &c.board).await? else {
                return Ok(None);
            };
            let list = posts::public_popular(ctx, actor, board.as_ref(), c.days, c.limit * c.pages)
                .await?;
            if board.is_none() {
                more_url = json!(recent_popular_url());
            }
            let items = summaries_any_board(state, &list, site).await?;
            json!({
                "posts": items,
                "days": c.days,
                "style": c.style,
                "page_size": c.limit,
                "pages": c.pages,
                "auto": c.auto,
            })
        }
        "post_tabs" => {
            let c: PostTabsConfig = cfg(w)?;
            let mut tabs = Vec::with_capacity(c.tabs.len());
            for (i, t) in c.tabs.iter().enumerate() {
                // 손님이 못 읽는 게시판의 탭은 뺀다.
                let Some(board) = readable(state, actor, &t.board).await? else {
                    continue;
                };
                let list = if t.source == "popular" {
                    posts::public_popular(ctx, actor, board.as_ref(), t.days, c.limit).await?
                } else {
                    let filter = PublicFilter {
                        exclude_secret: true,
                        ..PublicFilter::default()
                    };
                    posts::public_posts_where(
                        ctx,
                        actor,
                        board.as_ref(),
                        &filter,
                        PageReq::new(1, c.limit),
                    )
                    .await?
                    .items
                };
                let items = summaries_any_board(state, &list, site).await?;
                tabs.push(json!({
                    "id": format!("w-{}-t{i}", w.id),
                    "label": t.label,
                    "posts": items,
                    "more_url": match &board {
                        Some(b) => urls::path(&Target::Board { board: &b.slug }),
                        None if t.source == "popular" => recent_popular_url(),
                        None => urls::path(&Target::Recent),
                    },
                }));
            }
            if tabs.is_empty() {
                return Ok(None);
            }
            json!({ "tabs": tabs, "show_date": c.show_date })
        }
        "markdown_block" => {
            let c: MarkdownBlockConfig = cfg(w)?;
            let images = media::images_in(ctx, &c.body).await?;
            let html = markdown::render_with(&c.body, &|u| images.get(u).cloned());
            json!({ "html": html.into_string() })
        }
        "menu" => {
            let c: MenuConfig = cfg(w)?;
            let items = menu_value(state, &c.location).await;
            if items.as_array().is_none_or(Vec::is_empty) {
                return Ok(None);
            }
            json!({ "items": items })
        }
        "notice_bar" => {
            let c: NoticeBarConfig = cfg(w)?;
            json!({ "text": c.text, "link": link_json(&c.link) })
        }
        "search" => {
            let c: SearchConfig = cfg(w)?;
            json!({
                "action": urls::path(&Target::Search),
                "placeholder": if c.placeholder.is_empty() { "검색" } else { c.placeholder.as_str() },
            })
        }
        "account_box" => {
            let c: AccountBoxConfig = cfg(w)?;
            let intro = if c.intro.is_empty() {
                &site.description
            } else {
                &c.intro
            };
            json!({
                "intro": intro,
                "bar_url": "/me/bar",
                "login_url": "/login",
                "forgot_url": "/forgot",
                "signup_url": (site.signup == maru_core::model::SignupMode::Open).then_some("/signup"),
                "links": c.links.iter().map(|l| json!({ "label": l.label, "link": link_json(&l.link) })).collect::<Vec<_>>(),
            })
        }
        "image_banner" => {
            let c: ImageBannerConfig = cfg(w)?;
            let mut slides = Vec::new();
            for s in &c.slides {
                let Some(info) = media::image_info(ctx, &s.image).await? else {
                    continue;
                };
                let srcset = info
                    .variants
                    .iter()
                    .map(|(width, u)| format!("{u} {width}w"))
                    .chain(std::iter::once(format!("{} {}w", s.image, info.width)))
                    .collect::<Vec<_>>()
                    .join(", ");
                slides.push(json!({
                    "src": s.image,
                    "srcset": srcset,
                    "width": info.width,
                    "height": info.height,
                    "alt": s.alt,
                    "link": link_json(&s.link),
                }));
            }
            if slides.is_empty() {
                return Ok(None);
            }
            json!({ "slides": slides, "auto": c.auto })
        }
        "gallery_grid" => {
            let c: GalleryGridConfig = cfg(w)?;
            let Some(Some(board)) = readable(state, actor, &c.board).await? else {
                return Ok(None);
            };
            more_url = json!(urls::path(&Target::Board { board: &board.slug }));
            let filter = PublicFilter {
                exclude_secret: true,
                ..PublicFilter::default()
            };
            let list = posts::public_posts_where(
                ctx,
                actor,
                Some(&board),
                &filter,
                PageReq::new(1, c.limit),
            )
            .await?;
            let items = summaries_any_board(state, &list.items, site).await?;
            json!({ "posts": items, "columns": c.columns })
        }
        "popup" => {
            let c: PopupConfig = cfg(w)?;
            let images = media::images_in(ctx, &c.body).await?;
            let html = markdown::render_with(&c.body, &|u| images.get(u).cloned());
            let image = if c.image.is_empty() {
                Value::Null
            } else {
                match media::image_info(ctx, &c.image).await? {
                    Some(info) => {
                        json!({ "src": c.image, "width": info.width, "height": info.height })
                    }
                    None => Value::Null,
                }
            };
            json!({
                "html": html.into_string(),
                "image": image,
                "link": link_json(&c.link),
                "dismiss_days": c.dismiss_days,
                "position": c.position,
                // 내용이 바뀌면 「보지 않기」를 다시 묻도록 저장 키에 고친 시각을 섞는다.
                "key": format!("{}:{}", w.id, w.updated_at),
            })
        }
        // 코어 종류가 아니면 켜진 모듈의 것이다. 꺼진 모듈의 위젯은 조용히 빠진다 (SPEC §7.3).
        _ => {
            let Some(owner) = state.modules.widget_owner(ctx, &w.kind).await else {
                return Ok(None);
            };
            template = json!(format!("mod/{}/{}.html", owner.manifest().id, w.kind));
            module_label = owner
                .widget_types()
                .into_iter()
                .find(|t| t.id == w.kind)
                .map(|t| t.name);
            let v = owner.widget_view(ctx, &w.kind, &w.config).await?;
            more_url = v.get("more_url").cloned().unwrap_or(Value::Null);
            v
        }
    };
    Ok(Some(json!({
        "id": w.id,
        "kind": w.kind,
        "region": w.region,
        "title": w.title,
        // 제목이 없을 때 영역 이름(aria-label)으로 쓴다
        "label": if w.title.is_empty() { module_label.unwrap_or(widgets::kind_name(&w.kind)) } else { w.title.as_str() },
        "template": template,
        "more_url": more_url,
        "data": data,
    })))
}

/// 위젯이 가리키는 게시판. 빈 주소는 `Some(None)`(전체), 손님이 못 읽거나 없으면 `None`(위젯을 뺀다).
async fn readable(
    state: &AppState,
    actor: &Actor,
    slug: &str,
) -> Result<Option<Option<maru_core::model::Board>>> {
    if slug.is_empty() {
        return Ok(Some(None));
    }
    Ok(boards::readable(&state.ctx, actor, slug)
        .await
        .ok()
        .map(Some))
}

fn link_json(link: &str) -> Value {
    if link.is_empty() {
        return Value::Null;
    }
    json!({ "href": link, "external": !link.starts_with('/') })
}
