//! 공개 사이트. 비로그인 Actor 로 공개 조회 서비스(`posts::public_*`, `pages::public_*`)만 부른다.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{Extensions, HeaderMap, Method, StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use maru_core::Actor;
use maru_core::model::{
    Board, Category, Listing, MemberStatus, MenuItem, MenuLink, Page, PageReq, Post, SiteSettings,
};
use maru_core::search::Range;
use maru_core::services::search::{self, SearchInput, Sort};
use maru_core::services::widgets::Screen;
use maru_core::services::{
    anonymous_actor, boards, comments, design, files, media, menus, pages, posts, settings, tags,
    wiki,
};
use maru_core::urls::{self, Target};
use maru_core::{Code, Error, Result, blobs, clock, markdown, slug};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AppState, error};

pub fn routes() -> Router<AppState> {
    // 화면·피드·테마 자산은 매번 렌더하되 ETag 로 전송을 줄인다.
    // 미디어(영구 캐시)와 첨부(큰 본문)는 따로 둔다.
    let rendered = Router::new()
        .route("/", get(home))
        .route("/b/{board}", get(board))
        .route("/b/{board}/{post}", get(post))
        .route("/p/{page}", get(page))
        .route("/tags", get(tags_index))
        .route("/tags/{tag}", get(tag_page))
        .route("/recent", get(recent))
        .route("/search", get(search_page))
        .route("/u/{name}", get(profile_page))
        .route("/sitemap.xml", get(crate::feeds::sitemap))
        .route("/rss.xml", get(crate::feeds::rss))
        .route("/robots.txt", get(crate::feeds::robots))
        .route("/assets/tokens.css", get(tokens_css))
        .route("/assets/fonts.css", get(fonts_css))
        .route("/assets/{*path}", get(asset))
        .route_layer(middleware::from_fn(crate::etag::conditional));
    Router::new()
        .merge(rendered)
        .route("/media/{*path}", get(media_file))
        .route("/files/{id}", get(file_download))
        .route("/fonts/{slug}/{name}", get(font_file))
        .fallback(not_found)
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    page: Option<u32>,
    /// 게시판 분류 주소. 빈 값은 전체.
    category: Option<String>,
    /// 게시판 안 검색어 (GAP C5). 있으면 목록 대신 검색 결과.
    #[serde(default)]
    q: String,
    #[serde(default, rename = "in")]
    range: String,
    #[serde(default)]
    sort: String,
    /// 목록 순서 latest·views·comments (GAP Q7, ADR-0092). 검색 순서 `sort` 와 다르다.
    #[serde(default)]
    order: String,
    /// 방문자가 고른 보기 `list`·`card` (GAP Q21, ADR-0106). 목록·카드 게시판에서만
    #[serde(default)]
    view: String,
}

/// 템플릿에 넘기는 사이트 공통 값. 설정·메뉴를 못 읽어도 화면은 기본값으로 뜬다.
pub async fn site_json(state: &AppState) -> Value {
    let s = settings::site(&state.ctx).await.unwrap_or_else(|e| {
        tracing::warn!(error = ?e, "사이트 설정을 못 읽어 기본값을 씁니다");
        SiteSettings::default()
    });
    site_value(state, &s).await
}

pub(crate) async fn menu_value(state: &AppState, location: &str) -> Value {
    menus::get(&state.ctx, location)
        .await
        .map(|items| menu_json(&items))
        .unwrap_or_else(|_| json!([]))
}

async fn site_value(state: &AppState, s: &SiteSettings) -> Value {
    json!({
        "name": s.name,
        "description": s.description,
        "lang": s.lang,
        "url": state.ctx.site_url,
        "home_url": urls::path(&Target::Home),
        "rss_url": urls::path(&Target::Rss),
        "search_url": urls::path(&Target::Search),
        "show_search": s.show_search,
        "list_votes": s.list_votes,
        "stickers": s.stickers,
        "skin": s.skin,
        // 상대 시간 필터의 날짜 표시용 시간대(분) (ADR-0096)
        "tz": s.tz_offset_min,
        // 로그인 상태는 HTML 에 굽지 않고 스킨 스크립트가 따로 받는다 (ADR-0081)
        "account_bar_url": "/me/bar",
        // 가입을 받는 사이트만 머리글에 「내 계정」. 공개 화면은 손님 기준이라 로그인 여부와 상관없이 같다(ADR-0051).
        "show_account": s.signup != maru_core::model::SignupMode::Off,
        "account_url": "/me",
        "naver_verification": s.naver_verification,
        "google_verification": s.google_verification,
        "menus": {
            "header": menu_value(state, "header").await,
            "footer": menu_value(state, "footer").await,
            "quick": menu_value(state, "quick").await,
        },
    })
}

fn menu_json(items: &[MenuItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|i| {
                let (href, external) = match &i.link {
                    MenuLink::Home => (urls::path(&Target::Home), false),
                    MenuLink::Board { slug } => (urls::path(&Target::Board { board: slug }), false),
                    MenuLink::Page { slug } => (urls::path(&Target::Page { page: slug }), false),
                    MenuLink::Url { href } => (href.clone(), !href.starts_with('/')),
                };
                json!({
                    "label": i.label,
                    "href": href,
                    "external": external,
                    "children": menu_json(&i.children),
                })
            })
            .collect(),
    )
}

/// 요청마다 한 번 읽는 사이트 설정과 템플릿용 값.
struct Site {
    settings: SiteSettings,
    json: Value,
}

async fn load_site(state: &AppState) -> Result<Site> {
    let settings = settings::site(&state.ctx).await?;
    let json = site_value(state, &settings).await;
    Ok(Site { settings, json })
}

/// 공개 화면을 그렸다는 응답 표시. 방문자 통계가 이 응답만 센다 (ADR-0061).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Counted;

pub(crate) async fn render(state: &AppState, name: &str, ctx: Result<Value>) -> Response {
    let mut nonce = None;
    let rendered = match ctx {
        Ok(mut v) => {
            // 회원 이름 옆 표시 (GAP Q1): 글쓴이·댓글 칸이 있는 모든 화면에 붙는다.
            crate::modules::add_member_badges(state, &mut v).await;
            // 위젯 (ADR-0059): 화면 종류와 게시판·페이지 주소로 범위를 고른다.
            let board = v["board"]["slug"].as_str().map(str::to_owned);
            let page = v["page"]["slug"].as_str().map(str::to_owned);
            let screen = match name {
                "home.html" => Screen::Home,
                "board.html" | "post.html" => board.as_deref().map_or(Screen::Other, Screen::Board),
                "page.html" => page.as_deref().map_or(Screen::Other, Screen::Page),
                _ => Screen::Other,
            };
            let regions = crate::widgets::regions(state, screen).await;
            // 광고 코드처럼 바뀌는 도메인을 쓰는 위젯은 이 화면에만 nonce 정책을 건다 (ADR-0079).
            if wants_strict_csp(&regions) {
                nonce = maru_core::services::auth::new_secret()
                    .ok()
                    .map(|s| s[..32].to_owned());
            }
            if let Some(o) = v.as_object_mut() {
                o.insert("widgets".into(), regions);
                o.insert("csp_nonce".into(), json!(nonce));
            }
            state.theme.render(name, &v)
        }
        Err(e) => Err(e),
    };
    match rendered {
        Ok(html) => {
            let mut res = Html(html).into_response();
            res.extensions_mut().insert(Counted);
            if let Some(n) = nonce
                && let Ok(v) = header::HeaderValue::from_str(&strict_csp(&n))
            {
                res.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
            }
            res
        }
        Err(e) => error::page(state, &e).await,
    }
}

/// 영역 중 위젯 값에 `csp: "strict"` 가 있는가.
fn wants_strict_csp(regions: &Value) -> bool {
    regions.as_object().is_some_and(|o| {
        o.values()
            .filter_map(Value::as_array)
            .flatten()
            .any(|w| w["data"]["csp"] == "strict")
    })
}

/// 구글 애드센스 도움말이 권하는 strict CSP 에 우리 `frame-ancestors` 를 더한 것.
/// 광고 코드의 도메인은 바뀌므로 주소 목록 대신 nonce 와 `'strict-dynamic'` 으로 믿는다.
/// 이 화면의 모든 `<script>` 는 `csp_nonce` 를 단다.
fn strict_csp(nonce: &str) -> String {
    format!(
        "object-src 'none'; script-src 'nonce-{nonce}' 'unsafe-inline' 'unsafe-eval' 'strict-dynamic' https: http:; \
         base-uri 'none'; frame-ancestors 'self'"
    )
}

async fn home(State(state): State<AppState>, Query(q): Query<PageQuery>) -> Response {
    let v = home_inner(&state, q).await;
    render(&state, "home.html", v).await
}

async fn home_inner(state: &AppState, q: PageQuery) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let page = PageReq::new(q.page.unwrap_or(1), site.settings.posts_per_page);
    let list = posts::public_posts(&state.ctx, &actor, None, page).await?;
    let items = summaries_any_board(state, &list.items, &site.settings).await?;
    Ok(json!({
        "meta": json!({ "title": site.settings.name, "description": site.settings.description }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &Target::Home),
        "posts": items,
        "pagination": pagination(&list, &urls::path(&Target::Home)),
    }))
}

/// 여러 게시판에 걸친 목록(첫 화면·태그)의 항목. 글마다 게시판 주소를 찾아 붙인다.
pub(crate) async fn summaries_any_board(
    state: &AppState,
    list: &[Post],
    s: &SiteSettings,
) -> Result<Vec<Value>> {
    let all_boards = state.ctx.store.list_boards().await?;
    let items = list
        .iter()
        .map(|p| {
            let slug = all_boards
                .iter()
                .find(|b| b.id == p.board_id)
                .map(|b| b.slug.as_str())
                .unwrap_or_default();
            post_summary(slug, p, s)
        })
        .collect::<Vec<_>>();
    with_thumbs(state, items, list).await
}

async fn board(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<PageQuery>,
    Query(raw): Query<Vec<(String, String)>>,
) -> Response {
    // 여분 필드 거르기 `f.<키>=<값>` (GAP Q4, ADR-0089)
    let fields: Vec<(String, String)> = raw
        .into_iter()
        .filter_map(|(k, v)| k.strip_prefix("f.").map(|k| (k.to_owned(), v)))
        .collect();
    let v = board_inner(&state, &slug, q, &fields).await;
    render(&state, "board.html", v).await
}

async fn board_inner(
    state: &AppState,
    slug: &str,
    q: PageQuery,
    fields: &[(String, String)],
) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let board = boards::readable(&state.ctx, &actor, slug).await?;
    if !q.q.trim().is_empty() {
        return board_search(state, &site, &actor, &board, q).await;
    }
    let page = PageReq::new(q.page.unwrap_or(1), site.settings.posts_per_page);
    let category = q.category.as_deref().filter(|c| !c.is_empty());
    let sort = posts::ListSort::parse(&q.order);
    let found =
        posts::public_board_filtered(&state.ctx, &actor, &board, category, fields, sort, page)
            .await?;
    let cats = &found.categories;
    let today = clock::today(site.settings.tz_offset_min);
    // 보기 전환 (GAP Q21): 목록·카드 게시판만, 게시판 기본과 다를 때만 주소에 남긴다.
    let base_layout = board.effective_layout();
    let switchable = matches!(base_layout, "list" | "card");
    let layout = match q.view.as_str() {
        v @ ("list" | "card") if switchable => v.to_owned(),
        _ => base_layout.to_owned(),
    };
    let viewed = |path: String, v: &str| {
        if v == base_layout {
            path
        } else {
            urls::with_query(&path, &[("view", v)])
        }
    };
    let target = match &found.category {
        Some(c) => Target::Category {
            board: &board.slug,
            category: &c.slug,
        },
        None => Target::Board { board: &board.slug },
    };
    let summarize = |list: &[Post]| {
        list.iter()
            .map(|p| {
                let mut view = post_summary(&board.slug, p, &site.settings);
                view["category"] = category_of(&board.slug, cats, p);
                let (shown, flags) = list_fields(&board.field_schema, &p.meta);
                view["fields"] = json!(shown);
                view["flags"] = json!(flags);
                // 마감 칸이 있는 게시판(이벤트 등): D-day (GAP Q22, ADR-0106)
                if let Some(days) =
                    maru_core::fields::days_left(&board.field_schema, &p.meta, &today)
                {
                    view["deadline"] = json!({
                        "label": match days {
                            0 => "D-day".to_owned(),
                            d if d > 0 => format!("D-{d}"),
                            _ => "마감".to_owned(),
                        },
                        "closed": days < 0,
                    });
                }
                view
            })
            .collect::<Vec<_>>()
    };
    let notices = summarize(&found.notices);
    let mut items = summarize(&found.posts.items);
    // 목록 번호: 전체에서 거꾸로 센다(공지 제외).
    let first = found.posts.total
        - i64::from(found.posts.page.saturating_sub(1)) * i64::from(found.posts.per_page);
    // 조회·댓글 순이면 번호 칸에 순위를 보인다 (GAP Q16).
    let offset = i64::from(found.posts.page.saturating_sub(1)) * i64::from(found.posts.per_page);
    for (i, view) in items.iter_mut().enumerate() {
        view["number"] = json!(first - i as i64);
        if sort != posts::ListSort::Latest {
            view["rank"] = json!(offset + i as i64 + 1);
        }
    }
    let items = with_thumbs(state, items, &found.posts.items).await?;
    let current = found.category.as_ref().map(|c| c.id.as_str());
    let categories = cats
        .iter()
        .map(|c| {
            let mut v = category_json(&board.slug, c);
            v["current"] = json!(Some(c.id.as_str()) == current);
            v
        })
        .collect::<Vec<_>>();
    // 목록 순서 단추 (GAP Q7): 분류·거르기를 유지한다.
    let sorts = [
        (posts::ListSort::Latest, "최신"),
        (posts::ListSort::Views, "조회"),
        (posts::ListSort::Comments, "댓글"),
    ]
    .iter()
    .map(|(s, label)| {
        json!({
            "label": label,
            "url": filtered_path(&sorted_path(&viewed(urls::path(&target), &layout), *s), &found.filters, None),
            "current": *s == sort,
        })
    })
    .collect::<Vec<_>>();
    let title = match &found.category {
        Some(c) => format!("{} · {} · {}", c.name, board.name, site.settings.name),
        None => format!("{} · {}", board.name, site.settings.name),
    };
    let mut out = json!({
        "meta": json!({
            "title": title,
            "description": site.settings.description,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "board": board_json(&board),
        "categories": categories,
        "category": found.category.as_ref().map(|c| category_json(&board.slug, c)),
        "notices": notices,
        "posts": items,
        "pagination": pagination(&found.posts, &filtered_path(&sorted_path(&viewed(urls::path(&target), &layout), sort), &found.filters, None)),
        "field_filters": field_filters(&board.field_schema, &sorted_path(&viewed(urls::path(&target), &layout), sort), &found.filters),
        "sorts": sorts,
        "views": if switchable {
            json!(["list", "card"].iter().map(|v| json!({
                "label": if *v == "list" { "목록" } else { "카드" },
                "url": filtered_path(&sorted_path(&viewed(urls::path(&target), v), sort), &found.filters, None),
                "current": *v == layout,
            })).collect::<Vec<_>>())
        } else {
            Value::Null
        },
        "search": search_form(&urls::path(&Target::Board { board: &board.slug }), "", Range::All, Sort::Rank),
    });
    out["board"]["layout"] = json!(layout);
    // 위키 게시판: 필요한 문서·외톨이 문서 (GAP C13).
    if board.kind == "wiki" {
        out["wiki"] = json!(wiki::index_of(&state.ctx, &board).await?);
    }
    crate::modules::add_list_slots(state, &mut out).await;
    Ok(out)
}

/// 게시판 안 검색 결과 (`/b/{board}?q=`). 목록 자리에 결과를 보인다.
async fn board_search(
    state: &AppState,
    site: &Site,
    actor: &Actor,
    board: &Board,
    q: PageQuery,
) -> Result<Value> {
    let base = urls::path(&Target::Board { board: &board.slug });
    let params = SearchParams {
        q: q.q,
        range: q.range,
        sort: q.sort,
        page: q.page,
    };
    let (found, pager) = search_view(state, site, actor, &params, Some(board), &base).await?;
    Ok(json!({
        "meta": json!({
            "title": format!("“{}” 검색 · {} · {}", params.q.trim(), board.name, site.settings.name),
            "noindex": true,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &Target::Board { board: &board.slug }),
        "board": board_json(board),
        "categories": [],
        "category": Value::Null,
        "notices": [],
        "posts": [],
        "search": found,
        "pagination": pager,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: String,
    #[serde(default, rename = "in")]
    range: String,
    #[serde(default)]
    sort: String,
    page: Option<u32>,
}

const RANGES: [(Range, &str); 6] = [
    (Range::All, "제목+내용"),
    (Range::Title, "제목"),
    (Range::Body, "내용"),
    (Range::Tag, "태그"),
    (Range::Comment, "댓글"),
    (Range::Author, "글쓴이"),
];

/// 검색 칸 값 (아직 찾지 않은 상태).
fn search_form(action: &str, q: &str, range: Range, sort: Sort) -> Value {
    json!({
        "action": action,
        "q": q,
        "in": range.as_str(),
        "sort": sort.as_str(),
        "ranges": RANGES.iter().map(|(r, label)| json!({
            "value": r.as_str(),
            "label": label,
            "current": *r == range,
        })).collect::<Vec<_>>(),
        "searched": false,
    })
}

/// 검색 결과 화면 값 (`/search`·게시판 안 검색 공통). 검색어가 잘못되면 오류 화면 대신 안내를 넣는다.
async fn search_view(
    state: &AppState,
    site: &Site,
    actor: &Actor,
    p: &SearchParams,
    board: Option<&Board>,
    base: &str,
) -> Result<(Value, Value)> {
    let range = Range::parse(p.range.trim()).unwrap_or_default();
    let sort = Sort::parse(p.sort.trim()).unwrap_or_default();
    let q = p.q.trim();
    let mut view = search_form(base, q, range, sort);
    if q.is_empty() {
        return Ok((view, Value::Null));
    }
    view["searched"] = json!(true);
    // 댓글·글쓴이는 글자 그대로 찾고 늘 최신순이다 (ADR-0092).
    view["literal"] = json!(range.is_literal());
    let keep = |sort: Sort| {
        urls::with_query(
            base,
            &[
                ("q", q),
                (
                    "in",
                    if range == Range::All {
                        ""
                    } else {
                        range.as_str()
                    },
                ),
                (
                    "sort",
                    if sort == Sort::Rank {
                        ""
                    } else {
                        sort.as_str()
                    },
                ),
            ],
        )
    };
    view["sorts"] = json!([
        { "label": "정확도순", "url": keep(Sort::Rank), "current": sort == Sort::Rank },
        { "label": "최신순", "url": keep(Sort::New), "current": sort == Sort::New },
    ]);
    let input = SearchInput {
        q: q.to_owned(),
        board: board.map(|b| b.slug.clone()),
        range,
        sort,
        page: p.page.unwrap_or(1),
    };
    let found = match search::search(&state.ctx, actor, input).await {
        Ok(r) => r,
        Err(e) if e.code() == Code::Validation => {
            view["error"] = json!(e.message());
            view["hint"] = json!(e.hint());
            view["total"] = json!(0);
            view["items"] = json!([]);
            return Ok((view, Value::Null));
        }
        Err(e) => return Err(e),
    };
    let tz = site.settings.tz_offset_min;
    view["total"] = json!(found.total);
    view["items"] = Value::Array(
        found
            .items
            .iter()
            .map(|h| {
                let published = h.published_at.clone().unwrap_or_default();
                json!({
                    "kind": h.kind,
                    "title": h.title,
                    "url": h.url,
                    "board": h.board.as_ref().map(|b| json!({
                        "name": b.name,
                        "url": urls::path(&Target::Board { board: &b.slug }),
                    })),
                    "published_at": published,
                    "date": clock::display_date(&published, tz),
                    "snippet": h.snippet,
                    "tags": h.tags.iter().map(|name| json!({
                        "name": name,
                        "url": tag_url(&slug::tag(name)),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    );
    let pager = pages_json(i64::from(found.page), found.total_pages(), &keep(sort));
    Ok((view, pager))
}

async fn search_page(State(state): State<AppState>, Query(p): Query<SearchParams>) -> Response {
    let v = search_inner(&state, p).await;
    render(&state, "search.html", v).await
}

/// 회원 공개 프로필 (GAP Q2, ADR-0087). 손님 기준이라 모두에게 같은 화면이다.
async fn profile_page(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let v = profile_inner(&state, &name).await;
    render(&state, "profile.html", v).await
}

async fn profile_inner(state: &AppState, name: &str) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let p = maru_core::services::members::public_profile(&state.ctx, &actor, name).await?;
    let tz = site.settings.tz_offset_min;
    let posts: Vec<Value> = p
        .posts
        .iter()
        .map(|x| {
            json!({
                "title": x.title,
                "board": x.board_name,
                "url": x.url,
                "date": x.published_at.as_deref().map(|d| clock::display_date(d, tz)),
            })
        })
        .collect();
    let comments: Vec<Value> = p
        .comments
        .iter()
        .map(|c| {
            json!({
                "excerpt": c.excerpt,
                "post_title": c.post_title,
                "board": c.board_name,
                "url": c.url,
                "date": clock::display_date(&c.created_at, tz),
            })
        })
        .collect();
    Ok(json!({
        // 회원 화면은 검색 엔진에 내보내지 않는다.
        "meta": json!({ "title": format!("{} · {}", p.name, site.settings.name), "noindex": true }),
        "site": site.json,
        "profile": {
            "member_id": p.member_id,
            "name": p.name,
            "joined": clock::display_date(&p.joined_at, tz),
            "bio": p.bio,
            "post_count": p.post_count,
            "comment_count": p.comment_count,
            "posts": posts,
            "comments": comments,
        },
    }))
}

async fn search_inner(state: &AppState, p: SearchParams) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let base = urls::path(&Target::Search);
    let (found, pager) = search_view(state, &site, &actor, &p, None, &base).await?;
    let q = p.q.trim();
    let title = if q.is_empty() {
        format!("검색 · {}", site.settings.name)
    } else {
        format!("“{q}” 검색 · {}", site.settings.name)
    };
    Ok(json!({
        // 검색 결과는 검색 엔진에 내보내지 않는다.
        "meta": json!({ "title": title, "noindex": true }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &Target::Search),
        "search": found,
        "pagination": pager,
    }))
}

/// 목록 줄에 보일 여분 필드 (ADR-0089): 글자 칸들과, 켜진 참거짓 칸의 이름(표시).
fn list_fields(
    schema: &[maru_core::fields::FieldDef],
    meta: &serde_json::Map<String, Value>,
) -> (Vec<Value>, Vec<String>) {
    let listed: Vec<_> = schema.iter().filter(|d| d.list).cloned().collect();
    if listed.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut shown = Vec::new();
    let mut flags = Vec::new();
    for s in maru_core::fields::display(&listed, meta) {
        if s.kind == "boolean" {
            flags.push(s.label);
        } else {
            shown.push(json!({ "key": s.key, "label": s.label, "text": s.text }));
        }
    }
    (shown, flags)
}

/// 목록 순서를 붙인 주소 (기본 순서면 그대로).
fn sorted_path(base: &str, sort: posts::ListSort) -> String {
    if sort == posts::ListSort::Latest {
        base.to_owned()
    } else {
        urls::with_query(base, &[("order", sort.as_str())])
    }
}

/// 거르기를 유지한 주소. `change` 가 있으면 그 칸만 바꾸거나(값) 뺀다(`None` 값).
fn filtered_path(
    base: &str,
    active: &[(String, String)],
    change: Option<(&str, Option<&str>)>,
) -> String {
    let mut pairs: Vec<(String, String)> = active
        .iter()
        .filter(|(k, _)| change.is_none_or(|(c, _)| c != k))
        .map(|(k, v)| (format!("f.{k}"), v.clone()))
        .collect();
    if let Some((k, Some(v))) = change {
        pairs.push((format!("f.{k}"), v.to_owned()));
    }
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    urls::with_query(base, &refs)
}

/// 거르기 단추 묶음 (ADR-0089): 거를 수 있는 칸마다 「전체」와 값들.
fn field_filters(
    schema: &[maru_core::fields::FieldDef],
    base: &str,
    active: &[(String, String)],
) -> Vec<Value> {
    schema
        .iter()
        .filter(|d| d.filter)
        .map(|d| {
            let now = active
                .iter()
                .find(|(k, _)| *k == d.key)
                .map(|(_, v)| v.as_str());
            let mut choices = vec![json!({
                "label": "전체",
                "url": filtered_path(base, active, Some((&d.key, None))),
                "current": now.is_none(),
            })];
            let values: Vec<(String, String)> = if d.kind == "boolean" {
                vec![
                    ("0".to_owned(), format!("{} 빼기", d.label)),
                    ("1".to_owned(), format!("{}만", d.label)),
                ]
            } else if d.kind == "date" {
                vec![
                    ("open".to_owned(), "진행 중".to_owned()),
                    ("closed".to_owned(), "마감".to_owned()),
                ]
            } else {
                d.options
                    .iter()
                    .map(|o| {
                        let label = if o.label.is_empty() {
                            &o.value
                        } else {
                            &o.label
                        };
                        (o.value.clone(), label.clone())
                    })
                    .collect()
            };
            for (value, label) in values {
                choices.push(json!({
                    "label": label,
                    "url": filtered_path(base, active, Some((&d.key, Some(&value)))),
                    "current": now == Some(value.as_str()),
                }));
            }
            json!({ "key": d.key, "label": d.label, "choices": choices })
        })
        .collect()
}

fn category_json(board_slug: &str, c: &Category) -> Value {
    json!({
        "name": c.name,
        "slug": c.slug,
        "url": urls::path(&Target::Category { board: board_slug, category: &c.slug }),
    })
}

/// 글의 분류(없으면 null). 지운 분류를 가리키면 없는 것으로 본다.
fn category_of(board_slug: &str, cats: &[Category], p: &Post) -> Value {
    p.category_id
        .as_deref()
        .and_then(|id| cats.iter().find(|c| c.id == id))
        .map_or(Value::Null, |c| category_json(board_slug, c))
}

/// 글 화면 주소의 선택 값: 답글 대상, 방금 쓴 댓글이 승인 대기인지.
#[derive(Debug, Default, Deserialize)]
struct PostQuery {
    #[serde(default)]
    reply: String,
    #[serde(default)]
    comment: String,
    /// 댓글 순서 oldest·newest (GAP Q8, ADR-0093)
    #[serde(default)]
    corder: String,
}

async fn post(
    State(state): State<AppState>,
    Path((board_slug, post_slug)): Path<(String, String)>,
    Query(q): Query<PostQuery>,
    method: Method,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    let slot_headers = headers.clone();
    // 조회수: HEAD 는 세지 않는다. 연결 주소만 본다(전달 헤더는 믿지 않는다, rate_limit 과 같다).
    let visit = (method == Method::GET).then(|| Visit {
        ip: extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string())
            .unwrap_or_default(),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        headers,
    });
    let mut v = post_inner(&state, &board_slug, &post_slug, visit, &q).await;
    if let Ok(view) = &mut v {
        crate::modules::add_post_slots(&state, &slot_headers, view).await;
    }
    render(&state, "post.html", v).await
}

/// 글 화면 한 번의 방문 (조회수용).
struct Visit {
    ip: String,
    user_agent: String,
    headers: HeaderMap,
}

/// 글쓴이 카드 탭마다 보일 최근 글·댓글 수 (GAP Q30)
const AUTHOR_RECENT: usize = 5;

async fn post_inner(
    state: &AppState,
    board_slug: &str,
    post_slug: &str,
    visit: Option<Visit>,
    q: &PostQuery,
) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let (board, post) = posts::public_post(&state.ctx, &actor, board_slug, post_slug).await?;
    if let Some(v) = visit {
        let viewer = crate::admin::viewer(state, &v.headers).await;
        state
            .views
            .record(viewer.as_ref(), &post.id, &v.ip, &v.user_agent);
    }
    let target = Target::Post {
        board: &board.slug,
        post: &post.slug,
    };
    let mut view = post_summary(&board.slug, &post, &site.settings);
    if site.settings.show_views {
        view["views"] = json!(state.views.shown(&post));
    }
    if let Some(id) = &post.category_id
        && let Some(c) = state.ctx.store.category_by_id(id).await?
    {
        view["category"] = category_json(&board.slug, &c);
    }
    let near = posts::nearby(&state.ctx, &actor, &board, &post).await?;
    let neighbor = |p: &Option<Post>| {
        p.as_ref().map_or(Value::Null, |p| {
            post_summary(&board.slug, p, &site.settings)
        })
    };
    let nearby = json!({
        "prev": neighbor(&near.prev),
        "next": neighbor(&near.next),
        "related": summaries_any_board(state, &near.related, &site.settings).await?,
    });
    // 위키 게시판: `[[문서]]` 를 잇고, 이 문서를 가리키는 문서를 붙인다 (GAP C13).
    let doc = if board.kind == "wiki" {
        let links = wiki::links(&state.ctx, &board, &post.body_md).await?;
        view["wiki"] = json!({ "backlinks": wiki::backlinks(&state.ctx, &board, &post).await? });
        body_document(state, &post.body_md, &site.settings, Some(&links)).await?
    } else {
        body_document(state, &post.body_md, &site.settings, None).await?
    };
    view["body_html"] = Value::String(doc.html.into_string());
    view["toc"] = json!(doc.toc);
    let image = cover_image(state, &post).await?;
    let comments = comments_json(state, &actor, &board, &post, &site.settings, q).await?;
    view["author"] = authors(state, std::slice::from_ref(&post))
        .await?
        .pop()
        .unwrap_or(Value::Null);
    // 글 아래 글쓴이 카드 (GAP Q2): 이미 공개된 것(가입일·자기소개)만.
    if view["author"]["url"].is_string()
        && let Some(id) = post.author_id.as_deref()
        && let Some(m) = state.ctx.store.member_by_id(id).await?
    {
        view["author"]["joined"] = json!(clock::display_date(
            &m.created_at,
            site.settings.tz_offset_min
        ));
        view["author"]["bio"] = json!(state.ctx.store.member_bio(id).await?.unwrap_or_default());
        view["author"]["signature"] = json!(
            state
                .ctx
                .store
                .member_signature(id)
                .await?
                .unwrap_or_default()
        );
        // 카드의 최근 글·댓글 탭 (GAP Q30, ADR-0113): 프로필 화면과 같은 공개 범위(손님 기준), 지금 글은 뺀다.
        if let Some(name) = view["author"]["name"].as_str().map(str::to_owned)
            && let Ok(p) =
                maru_core::services::members::public_profile(&state.ctx, &actor, &name).await
        {
            let tz = site.settings.tz_offset_min;
            let recent_posts: Vec<Value> = p
                .posts
                .iter()
                .filter(|x| x.id != post.id)
                .take(AUTHOR_RECENT)
                .map(|x| {
                    json!({
                        "title": x.title,
                        "url": x.url,
                        "date": x.published_at.as_deref().map(|d| clock::display_date(d, tz)),
                    })
                })
                .collect();
            let recent_comments: Vec<Value> = p
                .comments
                .iter()
                .take(AUTHOR_RECENT)
                .map(|c| {
                    json!({
                        "excerpt": c.excerpt,
                        "post_title": c.post_title,
                        "url": c.url,
                        "date": clock::display_date(&c.created_at, tz),
                    })
                })
                .collect();
            view["author"]["post_count"] = json!(p.post_count);
            view["author"]["comment_count"] = json!(p.comment_count);
            view["author"]["recent_posts"] = json!(recent_posts);
            view["author"]["recent_comments"] = json!(recent_comments);
        }
    }
    // 회원·손님이 쓴 글만 공개 화면에서 고친다. 누가 고칠 수 있는지는 누를 때 본다.
    if public_write(&board) && (post.author_id.is_some() || post.guest_name.is_some()) {
        let base = urls::path(&target);
        view["edit_url"] = json!(format!("{base}/edit"));
        view["delete_url"] = json!(format!("{base}/delete"));
    }
    // 여분 칸 (ADR-0057). 지금 정의에 있는 키만, 정의 순서대로.
    let shown = maru_core::fields::display(&board.field_schema, &post.meta);
    // 출처 줄(글 머리)과 제휴 링크 고지 (GAP Q21, ADR-0106)
    if let Some(s) = shown.iter().find(|s| s.role.as_deref() == Some("source")) {
        let host = s
            .text
            .split("://")
            .nth(1)
            .unwrap_or(&s.text)
            .split('/')
            .next()
            .unwrap_or_default()
            .trim_start_matches("www.");
        view["source"] = json!({ "label": s.label, "host": host, "href": s.href });
    }
    view["sponsored"] = json!(shown.iter().any(|s| s.role.as_deref() == Some("sponsored")));
    view["fields"] = json!(shown);
    // 답변 단추: 회원(이하)에게 답변을 여는 게시판만 (ADR-0056). 운영자 답변은 관리 화면에서.
    if public_write(&board)
        && matches!(board.policy.reply.as_str(), "guest" | "member")
        && post.depth() < posts::MAX_REPLY_DEPTH
    {
        view["reply_url"] = json!(urls::with_query(
            &format!("/write/{}", board.slug),
            &[("reply", post.id.as_str())]
        ));
    }
    if let Some(parent) = match &post.parent_id {
        Some(id) => state.ctx.store.post_by_id(id).await?,
        None => None,
    }
    .filter(|p| p.status == maru_core::model::PostStatus::Published)
    {
        view["parent"] = json!({
            "title": parent.title,
            "url": urls::path(&Target::Post { board: &board.slug, post: &parent.slug }),
        });
    }
    // 글 아래 게시판 목록 (GAP Q19): 게시판 첫 쪽을 손님 기준으로. 목록을 못 보는 게시판이면 없다.
    let board_list = if site.settings.list_under_post {
        let first = PageQuery {
            page: None,
            category: None,
            q: String::new(),
            range: String::new(),
            sort: String::new(),
            order: String::new(),
            view: String::new(),
        };
        match Box::pin(board_inner(state, &board.slug, first, &[])).await {
            Ok(mut list) => {
                if let Some(items) = list["posts"].as_array_mut() {
                    for item in items.iter_mut().filter(|i| i["id"] == post.id.as_str()) {
                        item["current"] = json!(true);
                    }
                }
                json!({
                    "posts": list["posts"].take(),
                    "notices": list["notices"].take(),
                    "pagination": list["pagination"].take(),
                })
            }
            Err(_) => Value::Null,
        }
    } else {
        Value::Null
    };
    Ok(json!({
        "meta": json!({
            "title": format!("{} · {}", post.title, site.settings.name),
            "description": view["summary"],
            "image": image,
            "type": "article",
            "published_time": post.published_at,
            "modified_time": post.updated_at,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "board": board_json(&board),
        "post": view,
        "nearby": nearby,
        "comments": comments,
        "board_list": board_list,
    }))
}

/// 글 아래 댓글: 목록·폼 정책·답글 대상·승인 대기 안내 (ADR-0052). 손님 기준이라 화면이 모두에게 같다.
async fn comments_json(
    state: &AppState,
    actor: &Actor,
    board: &Board,
    post: &Post,
    s: &SiteSettings,
    q: &PostQuery,
) -> Result<Value> {
    let rules = comments::rules(board, s.captcha);
    // 잠긴 글은 화면에서 폼·답글을 닫는다. 서비스도 막는다 (ADR-0058).
    let open = rules.open && !post.is_locked;
    let order = comments::CommentOrder::parse(&q.corder);
    let list = comments::list_for_post_ordered(&state.ctx, actor, &post.id, order).await?;
    let published =
        |c: &comments::CommentView| c.status == maru_core::model::CommentStatus::Published;
    // 채택한 댓글이 지금 공개 상태일 때만 보인다.
    let accepted = post
        .accepted_comment_id
        .as_deref()
        .and_then(|id| list.iter().find(|c| c.id == id && published(c)));
    let can_accept = board.accepts_answers() && !post.is_locked;
    let reply_to = list
        .iter()
        .find(|c| c.id == q.reply && c.status == maru_core::model::CommentStatus::Published)
        .map(|c| json!({ "id": c.id, "name": c.author_name }));
    let items: Vec<Value> = list
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "depth": c.depth,
                "deleted": c.status == maru_core::model::CommentStatus::Deleted,
                "author_name": c.author_name,
                "is_guest": c.is_guest,
                "by_post_author": c.by_post_author,
                "member_id": if c.is_guest { None } else { c.author_id.as_deref() },
                "reply_to_name": c.reply_to_name,
                "body": c.body,
                "created_at": c.created_at,
                "date": clock::display_date(&c.created_at, s.tz_offset_min),
                "edited": c.updated_at != c.created_at,
                "reply_url": urls::with_query(
                    &urls::path(&Target::Post { board: &board.slug, post: &post.slug }),
                    &[("reply", &c.id)],
                ) + "#comment-form",
                "edit_url": format!("/c/{}/edit", c.id),
                "delete_url": format!("/c/{}/delete", c.id),
                "accepted": accepted.is_some_and(|a| a.id == c.id),
                // 고를 수 있는지는 누를 때 본다 (화면은 손님 기준).
                "accept_url": (can_accept && published(c)).then(|| format!("/c/{}/accept", c.id)),
            })
        })
        .collect();
    let count = list
        .iter()
        .filter(|c| c.status == maru_core::model::CommentStatus::Published)
        .count();
    Ok(json!({
        "rules": rules,
        "open": open,
        "locked": post.is_locked,
        "accepted": accepted.map(|c| json!({
            "id": c.id,
            "author_name": c.author_name,
            "body": c.body,
            "date": clock::display_date(&c.created_at, s.tz_offset_min),
        })),
        "items": items,
        "count": count,
        // 댓글이 둘 이상일 때만 순서 단추 (ADR-0093)
        "orders": (count > 1).then(|| {
            let base = urls::path(&Target::Post { board: &board.slug, post: &post.slug });
            [
                (comments::CommentOrder::Oldest, "등록순"),
                (comments::CommentOrder::Newest, "최신순"),
            ]
            .iter()
            .map(|(o, label)| json!({
                "label": label,
                "url": urls::with_query(
                    &base,
                    &[("corder", if *o == comments::CommentOrder::Oldest { "" } else { o.as_str() })],
                ) + "#comments",
                "current": *o == order,
            }))
            .collect::<Vec<_>>()
        }),
        "action": format!("{}/comments", urls::path(&Target::Post { board: &board.slug, post: &post.slug })),
        "reply_to": reply_to,
        "pending": q.comment == "pending",
        "login_url": urls::with_query("/login", &[("next", &urls::path(&Target::Post { board: &board.slug, post: &post.slug }))]),
    }))
}

async fn tags_index(State(state): State<AppState>) -> Response {
    let v = tags_index_inner(&state).await;
    render(&state, "tags.html", v).await
}

async fn tags_index_inner(state: &AppState) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let list = tags::list(&state.ctx, &actor).await?;
    let items = list
        .iter()
        .map(|t| json!({ "name": t.name, "url": tag_url(&t.slug), "posts": t.posts }))
        .collect::<Vec<_>>();
    Ok(json!({
        "meta": json!({
            "title": format!("태그 · {}", site.settings.name),
            "description": site.settings.description,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &Target::Tags),
        "tags": items,
    }))
}

async fn tag_page(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    let v = tag_page_inner(&state, &slug, q).await;
    render(&state, "tag.html", v).await
}

async fn tag_page_inner(state: &AppState, slug: &str, q: PageQuery) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let page = PageReq::new(q.page.unwrap_or(1), site.settings.posts_per_page);
    let (tag, list) = posts::public_tag(&state.ctx, &actor, slug, page).await?;
    let target = Target::Tag { tag: &tag.slug };
    let items = summaries_any_board(state, &list.items, &site.settings).await?;
    Ok(json!({
        "meta": json!({
            "title": format!("#{} · {}", tag.name, site.settings.name),
            "description": format!("'{}' 태그가 달린 글 {}개", tag.name, list.total),
            // 글이 없는 태그 화면은 검색에 내보내지 않는다.
            "noindex": list.total == 0,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "tag": json!({ "name": tag.name, "url": tag_url(&tag.slug), "posts": list.total }),
        "tags_url": urls::path(&Target::Tags),
        "posts": items,
        "pagination": pagination(&list, &urls::path(&target)),
    }))
}

/// 인기 모아보기가 보는 기간(일)과 개수
const RECENT_POPULAR_DAYS: u32 = 7;
const RECENT_POPULAR_LIMIT: u32 = 50;

/// 모든 게시판 모아보기 (GAP Q6, ADR-0091): 손님이 읽을 수 있는 게시판의 새 글(쪽 넘김) 또는
/// 최근 7일 인기 글(조회 → 댓글). 비밀글은 뺀다. 위젯 「더 보기」가 게시판 없이 모을 때 여기로 온다.
async fn recent(State(state): State<AppState>, Query(q): Query<PageQuery>) -> Response {
    let v = recent_inner(&state, q).await;
    render(&state, "recent.html", v).await
}

async fn recent_inner(state: &AppState, q: PageQuery) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let popular = q.sort == "popular";
    let base = urls::path(&Target::Recent);
    let (list, pager) = if popular {
        let list = posts::public_popular(
            &state.ctx,
            &actor,
            None,
            RECENT_POPULAR_DAYS,
            RECENT_POPULAR_LIMIT,
        )
        .await?;
        (list, Value::Null)
    } else {
        let page = PageReq::new(q.page.unwrap_or(1), site.settings.posts_per_page);
        let filter = posts::PublicFilter {
            exclude_secret: true,
            ..posts::PublicFilter::default()
        };
        let found = posts::public_posts_where(&state.ctx, &actor, None, &filter, page).await?;
        let pager = pagination(&found, &base);
        (found.items, pager)
    };
    let mut items = summaries_any_board(state, &list, &site.settings).await?;
    // 어느 게시판 글인지 줄마다 보인다.
    let all_boards = state.ctx.store.list_boards().await?;
    for (view, p) in items.iter_mut().zip(&list) {
        if let Some(b) = all_boards.iter().find(|b| b.id == p.board_id) {
            view["board"] = json!({
                "name": b.name,
                "url": urls::path(&Target::Board { board: &b.slug }),
            });
        }
    }
    let title = if popular { "인기 글" } else { "새 글" };
    let mut out = json!({
        "meta": json!({
            "title": format!("{title} · {}", site.settings.name),
            "description": site.settings.description,
            "noindex": popular,
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &Target::Recent),
        "recent": {
            "title": title,
            "popular": popular,
            "days": RECENT_POPULAR_DAYS,
            "latest_url": base,
            "popular_url": urls::with_query(&base, &[("sort", "popular")]),
        },
        "posts": items,
        "pagination": pager,
    });
    crate::modules::add_list_slots(state, &mut out).await;
    Ok(out)
}

fn tag_url(slug: &str) -> String {
    urls::path(&Target::Tag { tag: slug })
}

async fn page(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let v = page_inner(&state, &slug).await;
    render(&state, "page.html", v).await
}

async fn page_inner(state: &AppState, slug: &str) -> Result<Value> {
    let site = load_site(state).await?;
    let actor = anonymous_actor(&state.ctx).await?;
    let p = pages::public_page(&state.ctx, &actor, slug).await?;
    let target = Target::Page { page: &p.slug };
    Ok(json!({
        "meta": json!({
            "title": format!("{} · {}", p.title, site.settings.name),
            "description": markdown::plain_text(&p.body_md, markdown::SUMMARY_CHARS),
        }),
        "site": site.json,
        "canonical": urls::absolute(&state.ctx.site_url, &target),
        "page": page_json(&p, body_document(state, &p.body_md, &site.settings, None).await?),
    }))
}

/// 디자인 토큰 CSS. 토큰은 검증된 값만 들어 있어 그대로 내보낸다.
async fn tokens_css(State(state): State<AppState>) -> Response {
    let tokens = design::tokens(&state.ctx).await.unwrap_or_default();
    let fonts = maru_core::services::fonts::list(&state.ctx)
        .await
        .unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        tokens.to_css(&fonts),
    )
        .into_response()
}

/// 올린 서체의 `@font-face`·`.ff-{slug}` (ADR-0048). slug·주소는 정해진 모양이라 그대로 내보낸다.
async fn fonts_css(State(state): State<AppState>) -> Response {
    let list = maru_core::services::fonts::list(&state.ctx)
        .await
        .unwrap_or_default();
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        maru_core::services::fonts::css(&list),
    )
        .into_response()
}

/// 올린 서체 파일과 라이선스 전문. 파일 이름에 내용 해시가 들어 있어 오래 캐시한다.
async fn font_file(
    State(state): State<AppState>,
    Path((slug, name)): Path<(String, String)>,
) -> Response {
    use maru_core::services::fonts;
    match fonts::file(&state.ctx, &slug, &name).await {
        Ok((format, bytes)) => {
            let cache = if format == "txt" {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
            };
            (
                [
                    (header::CONTENT_TYPE, fonts::mime(&format)),
                    (header::CACHE_CONTROL, cache),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) if e.code() == Code::NotFound => {
            (StatusCode::NOT_FOUND, "not found").into_response()
        }
        Err(e) => {
            tracing::error!(error = ?e, "서체 파일 읽기 실패");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn asset(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    // 내장 폰트는 내용이 바뀌면 파일 이름의 번호를 올리므로(maru-pen-1.woff2) 오래 캐시한다.
    // 스티커 그림도 같은 이름에 다른 그림을 넣지 않는다 (ADR-0115). 글에 저장된 주소라 바꾸면 안 된다.
    let cache = if (path.starts_with("fonts/") && path.ends_with(".woff2"))
        || (path.starts_with("stickers/") && path.ends_with(".png"))
    {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    match state.theme.asset(&path) {
        Some((bytes, mime)) => (
            [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
            bytes.into_owned(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 업로드한 이미지. 파일 이름에 ID 가 들어 있어 내용이 바뀌지 않으므로 오래 캐시한다.
/// 확장자로 정한 형식만 내보내고, 보관소 경로 규칙에 맞지 않으면 파일 시스템을 보지 않는다.
async fn media_file(State(state): State<AppState>, Path(path): Path<String>) -> Response {
    let mime = match path.rsplit_once('.').map(|(_, e)| e) {
        Some("jpg") => "image/jpeg",
        Some("png") => "image/png",
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    if !blobs::valid_path(&path) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match state.ctx.blobs.get(&path).await {
        Ok(Some(bytes)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "미디어 파일 읽기 실패");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// 첨부 파일은 항상 내려받기로, 정해진 MIME 으로만 보낸다 (브라우저 안에서 열리지 않는다).
async fn file_download(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match files::download(&state.ctx, &id).await {
        Ok((file, bytes)) => (
            [
                (header::CONTENT_TYPE, file.mime.clone()),
                (
                    header::CONTENT_DISPOSITION,
                    content_disposition(&file.filename),
                ),
                (header::CACHE_CONTROL, "public, max-age=86400".to_owned()),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox".to_owned(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) if e.code() == Code::NotFound => error::page(&state, &e).await,
        Err(e) => {
            tracing::error!(error = ?e, "첨부 파일 읽기 실패");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `attachment; filename="<ASCII 대체>"; filename*=UTF-8''<퍼센트 인코딩>` (RFC 6266).
fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c == ' ' || (c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '%')) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

async fn not_found(State(state): State<AppState>) -> Response {
    error::page(&state, &Error::new(Code::NotFound)).await
}

fn board_json(board: &Board) -> Value {
    json!({
        // 목록 줄에 작은 그림 (GAP Q21): 목록에 보이는 여분 칸이 있는 게시판(핫딜·장터 등)
        "row_thumbs": board.field_schema.iter().any(|d| d.list),
        "description": board.description,
        "slug": board.slug,
        "name": board.name,
        "kind": board.kind,
        // 목록 형태 (ADR-0054): list·webzine·card·gallery
        "layout": board.effective_layout(),
        // 목록 순서 (ADR-0058): latest·activity
        "order": board.effective_order(),
        "url": urls::path(&Target::Board { board: &board.slug }),
        // 회원·손님이 공개 화면에서 쓰는 게시판이면 글쓰기 단추 (ADR-0053). 권한은 누를 때 본다.
        "write_url": public_write(board).then(|| format!("/write/{}", board.slug)),
    })
}

/// 게시판 정책이 회원(이하)에게 글쓰기를 연다.
pub(crate) fn public_write(board: &Board) -> bool {
    matches!(board.policy.write.as_str(), "guest" | "member")
}

/// 글쓴이 표시: 회원 닉네임 또는 손님 이름. 운영자·에이전트가 관리 경로로 쓴 글은 없다(블로그처럼).
async fn authors(state: &AppState, posts: &[Post]) -> Result<Vec<Value>> {
    // 회원 ID → (닉네임, 프로필이 열리는지). 탈퇴·비활성 회원은 프로필 링크가 없다.
    let mut names: std::collections::HashMap<String, (String, bool)> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(posts.len());
    for p in posts {
        let v = match (&p.guest_name, &p.author_id) {
            (Some(name), _) => json!({ "name": name, "guest": true }),
            (None, Some(id)) => {
                if !names.contains_key(id)
                    && let Some(m) = state.ctx.store.member_by_id(id).await?
                {
                    let open = m.status == MemberStatus::Active && m.withdrawn_at.is_none();
                    names.insert(id.clone(), (m.display_name, open));
                }
                names.get(id).map_or(Value::Null, |(n, open)| {
                    json!({
                        "name": n,
                        "guest": false,
                        "member_id": id,
                        "url": open.then(|| urls::path(&Target::Member { name: n })),
                    })
                })
            }
            (None, None) => Value::Null,
        };
        out.push(v);
    }
    Ok(out)
}

/// 목록 썸네일: 대표 이미지의 가장 작은 축소본(없으면 원본)과 원본 크기.
/// 보관소에서 찾지 못한 사진(지워진 것 등)은 깨진 그림이 되지 않게 빼 둔다.
async fn with_thumbs(
    state: &AppState,
    mut items: Vec<Value>,
    posts: &[Post],
) -> Result<Vec<Value>> {
    // 공개 댓글 수도 여기서 붙인다 (목록마다 한 번의 쿼리).
    let ids: Vec<String> = posts.iter().map(|p| p.id.clone()).collect();
    let counts = comments::counts(&state.ctx, &ids).await?;
    let names = authors(state, posts).await?;
    for ((view, p), author) in items.iter_mut().zip(posts).zip(names) {
        if let Some(n) = counts.get(&p.id) {
            view["comments"] = json!(n);
        }
        view["author"] = author;
    }
    for (view, p) in items.iter_mut().zip(posts) {
        let Some(url) = posts::cover_of(p) else {
            continue;
        };
        let Some(info) = media::image_info(&state.ctx, &url).await? else {
            continue;
        };
        let src = info
            .variants
            .iter()
            .min_by_key(|(w, _)| *w)
            .map(|(_, u)| u.clone())
            .unwrap_or(url);
        view["thumb"] = json!({ "src": src, "width": info.width, "height": info.height });
    }
    Ok(items)
}

/// 공유 미리보기(`og:image`)용 절대 주소. 가장 큰 축소본, 없으면 원본.
async fn cover_image(state: &AppState, post: &Post) -> Result<Option<String>> {
    let Some(url) = posts::cover_of(post) else {
        return Ok(None);
    };
    let Some(info) = media::image_info(&state.ctx, &url).await? else {
        return Ok(None);
    };
    let best = info
        .variants
        .iter()
        .max_by_key(|(w, _)| *w)
        .map(|(_, u)| u.clone())
        .unwrap_or(url);
    Ok(Some(format!(
        "{}{best}",
        state.ctx.site_url.trim_end_matches('/')
    )))
}

/// 본문 HTML 과 목차. 보관소 그림에는 크기와 축소본(srcset)을 붙인다.
/// 제목 수가 사이트 설정의 기준에 못 미치면(또는 설정이 0 이면) 목차는 비운다.
async fn body_document(
    state: &AppState,
    md: &str,
    s: &SiteSettings,
    wiki: Option<&HashMap<String, markdown::WikiHref>>,
) -> Result<markdown::Document> {
    let images = media::images_in(&state.ctx, md).await?;
    let image = |url: &str| images.get(url).cloned();
    let mut doc = match wiki {
        Some(links) => markdown::render_wiki_document(md, &image, &|t| {
            links
                .get(&markdown::wiki_key(t))
                .cloned()
                .unwrap_or_else(|| markdown::WikiHref {
                    href: "#".into(),
                    missing: true,
                })
        }),
        None => markdown::render_document(md, &image),
    };
    // 링크 카드 (ADR-0084). 정보를 못 읽어도 본문은 그린다 — 카드는 주소와 도메인만 보인다.
    let link_urls = markdown::link_urls(md);
    if !link_urls.is_empty() {
        let cards = maru_core::services::links::cards_for(&state.ctx, &link_urls)
            .await
            .unwrap_or_default();
        doc.html = markdown::fill_link_cards(doc.html, &|url| {
            let sns = maru_core::services::links::sns_label(url);
            match cards.get(url) {
                Some(p) => Some(markdown::LinkCardInfo {
                    title: p.title.clone(),
                    description: p.description.clone(),
                    site: p.site_name.clone().or_else(|| sns.map(str::to_owned)),
                    image: p.image_url.clone(),
                }),
                None => sns.map(|s| markdown::LinkCardInfo {
                    site: Some(s.to_owned()),
                    ..markdown::LinkCardInfo::default()
                }),
            }
        });
    }
    let min = s.toc_min_headings as usize;
    if min == 0 || doc.toc.len() < min {
        doc.toc.clear();
    }
    Ok(doc)
}

fn page_json(p: &Page, doc: markdown::Document) -> Value {
    json!({
        "title": p.title,
        "slug": p.slug,
        "layout": p.layout,
        "url": urls::path(&Target::Page { page: &p.slug }),
        "summary": markdown::plain_text(&p.body_md, markdown::SUMMARY_CHARS),
        "body_html": doc.html.into_string(),
        "toc": doc.toc,
    })
}

/// 「새 글」 표시 기간.
const NEW_HOURS: i64 = 24;

fn post_summary(board_slug: &str, p: &Post, s: &SiteSettings) -> Value {
    let published = p.published_at.clone().unwrap_or_default();
    let summary = p
        .excerpt
        .clone()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| markdown::plain_text(&p.body_md, markdown::SUMMARY_CHARS));
    json!({
        "id": p.id,
        "title": p.title,
        "slug": p.slug,
        "url": urls::path(&Target::Post { board: board_slug, post: &p.slug }),
        "published_at": published,
        "date": clock::display_date(&published, s.tz_offset_min),
        "summary": summary,
        "is_notice": p.is_notice,
        "is_secret": p.is_secret,
        // 포럼 (ADR-0058): 잠긴 글, 채택된 답변이 있는 글, 마지막 활동 시각
        "is_locked": p.is_locked,
        "solved": p.accepted_comment_id.is_some(),
        "activity_at": p.last_activity_at.clone().unwrap_or_else(|| published.clone()),
        "activity_date": clock::display_date(
            p.last_activity_at.as_deref().unwrap_or(&published),
            s.tz_offset_min,
        ),
        // 답변글 (ADR-0056). 목록은 스레드 순서로 오고 깊이만큼 들여 쓴다.
        "reply": p.parent_id.is_some(),
        "depth": p.depth(),
        // 발행 24시간 안 (ADR-0054). 본문 해시 ETag 라 시간이 지나면 응답이 저절로 바뀐다.
        "is_new": !published.is_empty() && published > clock::hours_ago(NEW_HOURS),
        // 그림 없는 카드·갤러리 칸에 쓰는 글자
        "initial": p.title.chars().next().map(String::from).unwrap_or_default(),
        // 사이트 설정으로 숨기면 null. 목록은 DB 값이라 몇십 초 늦을 수 있다.
        "views": if s.show_views { json!(p.view_count) } else { Value::Null },
        // 비밀글은 서비스가 이미 태그를 비워서 준다.
        "tags": p.tags.iter().map(|name| json!({
            "name": name,
            "url": tag_url(&slug::tag(name)),
        })).collect::<Vec<_>>(),
        "category": Value::Null,
    })
}

/// `base` 는 첫 쪽 주소. 이미 쿼리(`?category=`)가 있으면 `&page=` 로 잇는다.
pub(crate) fn pagination<T>(list: &Listing<T>, base: &str) -> Value {
    pages_json(i64::from(list.page), list.total_pages(), base)
}

fn pages_json(page: i64, total_pages: i64, base: &str) -> Value {
    let sep = if base.contains('?') { '&' } else { '?' };
    let link = |n: i64| {
        if n == 1 {
            base.to_owned()
        } else {
            format!("{base}{sep}page={n}")
        }
    };
    // 번호는 10개씩 묶는다(1~10, 11~20 …). 지금 쪽에서 셋 이상 먼 번호는 `far` —
    // 좁은 화면 스킨이 숨겨 다섯 개만 보이게 한다 (GAP Q16).
    let start = (page.max(1) - 1) / PAGE_BLOCK * PAGE_BLOCK + 1;
    let end = (start + PAGE_BLOCK - 1).min(total_pages);
    let pages = (start..=end)
        .map(|n| {
            json!({
                "n": n,
                "url": link(n),
                "current": n == page,
                "far": (n - page).abs() > 2,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "page": page,
        "total_pages": total_pages,
        "prev_url": (page > 1).then(|| link(page - 1)),
        "next_url": (page < total_pages).then(|| link(page + 1)),
        "first_url": (page > 1).then(|| link(1)),
        "last_url": (page < total_pages).then(|| link(total_pages)),
        "pages": pages,
    })
}

/// 쪽 번호 한 묶음의 크기
const PAGE_BLOCK: i64 = 10;
