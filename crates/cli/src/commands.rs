//! 명령 구현. 모든 명령은 `maru_core::services` 를 부르고 결과를 출력 계약으로 낸다.

use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use maru_core::actor::Scope;
use maru_core::config::{Config, DEFAULT_BIND};
use maru_core::model::{MenuItem, MenuLink, Page, PageReq, PageStatus, PostStatus};
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::tokens::{self, NewToken};
use maru_core::services::{menus, pages, settings};
use maru_core::urls::{self, Target};
use maru_core::{Actor, Code, Ctx, Error, Result};
use maru_storage::{DataDir, Db};
use serde_json::{Value, json};

use crate::output;
use crate::{
    BoardCmd, Command, Global, InitArgs, MenuCmd, MenuSetArgs, PageCmd, PostCmd, PostListArgs,
    PostNewArgs, ServeArgs, SiteCmd, TokenCmd, TokenCreateArgs,
};

pub async fn run(g: &Global, command: Command) -> ExitCode {
    let res = match command {
        Command::Init(args) => init(g, args).await,
        Command::Serve(args) => return serve(g, args).await,
        Command::Post(cmd) => post(g, cmd).await,
        Command::Board(cmd) => board(g, cmd).await,
        Command::Token(cmd) => token(g, cmd).await,
        Command::Page(cmd) => page(g, cmd).await,
        Command::Menu(cmd) => menu(g, cmd).await,
        Command::Site(cmd) => site(g, cmd).await,
    };
    match res {
        Ok((data, human)) => output::success(data, g.json, |_| human),
        Err(e) => output::error(&e, g.json),
    }
}

type Out = Result<(Value, String)>;

/// 로컬 CLI 는 운영자다. `MARU_TOKEN` 이 있으면 그 토큰의 스코프로 제한한다 (Q2).
async fn actor(ctx: &Ctx) -> Result<Actor> {
    match std::env::var("MARU_TOKEN") {
        Ok(t) if !t.trim().is_empty() => tokens::authenticate(ctx, t.trim()).await,
        _ => Ok(Actor::local()),
    }
}

async fn open(g: &Global) -> Result<(Ctx, Config, Db)> {
    maru_storage::open(&DataDir::new(&g.data)).await
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(Error::internal)
}

// ---- init ----

async fn init(g: &Global, args: InitArgs) -> Out {
    if args.preset != "blog" {
        return Err(Error::new(Code::Validation)
            .with_message("error.preset_unknown")
            .arg("value", args.preset)
            .next("maru init --preset blog --yes"));
    }
    let dir = DataDir::new(&g.data);
    dir.ensure_layout()?;
    let fresh = !dir.is_initialized();

    let bind = args.bind.unwrap_or_else(|| DEFAULT_BIND.to_owned());
    let site_url = args.site_url.unwrap_or_else(|| format!("http://{bind}"));
    let config = Config { bind, site_url }.validated()?;
    let wrote_config = dir.write_config_if_absent(&config)?;
    let config = dir.read_config()?;

    let db = Db::open(&dir.db_path(), true).await?;
    db.migrate().await?;
    let ctx = Ctx {
        store: Arc::new(maru_storage::SqliteStore::new(db.clone())),
        site_url: config.site_url.clone(),
    };
    let local = Actor::local();
    let board = boards::ensure(
        &ctx,
        &local,
        NewBoard {
            slug: "blog".into(),
            name: "블로그".into(),
            kind: "blog".into(),
            policy: None,
            sort_order: 0,
        },
    )
    .await?;
    if let Some(name) = args.name {
        let patch = settings::SettingsPatch {
            name: Some(name),
            ..Default::default()
        };
        settings::update(&ctx, &local, patch).await?;
    }
    // 기본 메뉴는 처음 만들 때만 깐다. 다시 실행해도 사용자가 고친 메뉴를 덮지 않는다.
    if fresh {
        let header = vec![MenuItem {
            label: "글".into(),
            link: MenuLink::Board {
                slug: "blog".into(),
            },
            children: vec![],
        }];
        menus::set(&ctx, &local, "header", header).await?;
    }
    // 초기 토큰은 DB 를 처음 만들 때만 발급한다. 다시 실행해도 토큰이 늘지 않는다.
    let token = if fresh {
        Some(
            tokens::create(
                &ctx,
                &local,
                NewToken {
                    name: "init".into(),
                    scopes: vec![],
                    board_ids: None,
                    expires_at: None,
                },
            )
            .await?,
        )
    } else {
        None
    };
    db.close().await;

    let data_dir = dir.root().display().to_string();
    let serve_cmd = format!("maru serve --data \"{data_dir}\"");
    let mut next = vec![serve_cmd.clone()];
    next.push(format!(
        "maru post new --data \"{data_dir}\" --title \"첫 글\" --from post.md --publish"
    ));
    let data = json!({
        "data_dir": data_dir,
        "created": fresh,
        "config_written": wrote_config,
        "site_url": config.site_url,
        "bind": config.bind,
        "board": to_value(&board)?,
        "token": token.as_ref().map(|t| json!({
            "id": t.token.id,
            "name": t.token.name,
            "scopes": t.token.scopes,
            "secret": t.secret,
        })),
        "next": next,
    });
    let mut human = if fresh {
        format!("사이트를 만들었습니다: {data_dir}\n")
    } else {
        format!("이미 초기화된 사이트입니다. 빠진 항목만 채웠습니다: {data_dir}\n")
    };
    if let Some(t) = &token {
        human.push_str(&format!(
            "API 토큰 (read, draft). 다시 볼 수 없으니 지금 보관하세요:\n  {}\n",
            t.secret
        ));
    }
    human.push_str(&format!("다음: {serve_cmd}"));
    Ok((data, human))
}

// ---- serve ----

async fn serve(g: &Global, args: ServeArgs) -> ExitCode {
    match serve_inner(g, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => output::error(&e, g.json),
    }
}

async fn serve_inner(g: &Global, args: ServeArgs) -> Result<()> {
    let dir = DataDir::new(&g.data);
    let (ctx, config, db) = maru_storage::open(&dir).await?;
    let bind = args.bind.unwrap_or(config.bind);
    let addr: SocketAddr = bind.parse().map_err(|_| {
        Error::new(Code::Validation)
            .with_message("error.config_bind")
            .arg("value", bind.clone())
    })?;
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let mut other = addr;
            other.set_port(addr.port().wrapping_add(1).max(1024));
            return Err(Error::new(Code::PortInUse)
                .arg("port", addr.port().to_string())
                .with_hint("hint.port_in_use")
                .next(format!(
                    "maru serve --data \"{}\" --bind {other}",
                    dir.root().display()
                )));
        }
        Err(e) => return Err(Error::internal(e)),
    };
    let local = listener.local_addr().map_err(Error::internal)?;
    let url = format!("http://{local}");

    let theme_dir = dir.themes_dir().join("default");
    let theme = maru_themes::Theme::new(theme_dir.is_dir().then_some(theme_dir));
    let state = maru_web::AppState {
        ctx,
        theme: Arc::new(theme),
    };
    let started = json!({ "url": url, "bind": local.to_string(), "site_url": config.site_url });
    let human = format!("서버를 시작했습니다: {url}\n멈추려면 Ctrl+C");
    output::success(started, g.json, |_| human);
    if !g.quiet {
        tracing::info!(%url, "serving");
    }

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let res = maru_web::serve(state, listener, shutdown).await;
    db.close().await;
    res
}

// ---- post ----

async fn post(g: &Global, cmd: PostCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        PostCmd::New(args) => post_new(&ctx, &actor, args).await,
        PostCmd::Publish { id } => {
            let p = posts::publish(&ctx, &actor, &id).await?;
            post_out(&ctx, &p, "발행했습니다").await
        }
        PostCmd::Submit { id } => {
            let p = posts::submit(&ctx, &actor, &id).await?;
            post_out(&ctx, &p, "검토를 요청했습니다").await
        }
        PostCmd::Hide { id } => {
            let p = posts::hide(&ctx, &actor, &id).await?;
            post_out(&ctx, &p, "숨겼습니다").await
        }
        PostCmd::Delete { id } => {
            let p = posts::delete(&ctx, &actor, &id).await?;
            let (data, human) = post_out(&ctx, &p, "삭제했습니다").await?;
            Ok((
                data,
                format!("{human}\n되돌리기: maru post restore {}", p.id),
            ))
        }
        PostCmd::Restore { id } => {
            let p = posts::restore(&ctx, &actor, &id).await?;
            post_out(&ctx, &p, "되돌렸습니다").await
        }
        PostCmd::Edit(args) => {
            let body_md = match (&args.body, &args.from) {
                (Some(b), _) => Some(b.clone()),
                (None, Some(path)) => Some(read_body(path)?),
                (None, None) => None,
            };
            let patch = posts::PostPatch {
                title: args.title,
                slug: args.slug,
                body_md,
                excerpt: args.excerpt,
            };
            let p = posts::update(&ctx, &actor, &args.id, patch).await?;
            post_out(&ctx, &p, "고쳤습니다").await
        }
        PostCmd::List(args) => post_list(&ctx, &actor, args).await,
        PostCmd::Show { id } => {
            let p = posts::get(&ctx, &actor, &id).await?;
            let human = format!("[{}] {}\n\n{}", p.status.as_str(), p.title, p.body_md);
            Ok((to_value(&p)?, human))
        }
    };
    db.close().await;
    res
}

/// 글 결과에 공개 URL 을 붙여 낸다.
async fn post_out(ctx: &Ctx, p: &maru_core::model::Post, verb: &str) -> Out {
    let url = post_url(ctx, &p.board_id, &p.slug).await?;
    let mut data = to_value(p)?;
    data["url"] = Value::String(url.clone());
    Ok((
        data,
        format!("{verb}: {} [{}]\n{url}", p.title, p.status.as_str()),
    ))
}

async fn post_new(ctx: &Ctx, actor: &Actor, args: PostNewArgs) -> Out {
    let body_md = match (&args.body, &args.from) {
        (Some(b), _) => b.clone(),
        (None, Some(path)) => read_body(path)?,
        (None, None) => String::new(),
    };
    let input = NewPost {
        board: args.board,
        title: args.title,
        slug: args.slug,
        body_md,
        excerpt: args.excerpt,
    };
    let (mut p, created) = if args.if_not_exists {
        let res = posts::ensure_draft(ctx, actor, input).await?;
        (res.item, res.created)
    } else {
        (posts::create_draft(ctx, actor, input).await?, true)
    };
    if args.publish {
        p = posts::publish(ctx, actor, &p.id).await?;
    }
    let url = post_url(ctx, &p.board_id, &p.slug).await?;
    let mut data = to_value(&p)?;
    data["url"] = Value::String(url.clone());
    data["created"] = Value::Bool(created);
    let verb = match (created, args.publish) {
        (false, _) => "이미 있습니다",
        (true, true) => "발행",
        (true, false) => "초안 저장",
    };
    let mut human = format!("{verb}: {} (id {})\n{url}", p.title, p.id);
    if !args.publish && created {
        human.push_str(&format!("\n다음: maru post publish {}", p.id));
    }
    Ok((data, human))
}

async fn post_list(ctx: &Ctx, actor: &Actor, args: PostListArgs) -> Out {
    let status = match args.status.as_deref() {
        Some(s) => Some(PostStatus::parse(s).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.post_status")
                .arg("value", s)
        })?),
        None => None,
    };
    let list = posts::list(
        ctx,
        actor,
        args.board.as_deref(),
        status,
        PageReq::new(args.page, args.per_page),
    )
    .await?;
    let mut human = format!("글 {}개", list.total);
    for p in &list.items {
        human.push_str(&format!("\n{}  [{}]  {}", p.id, p.status.as_str(), p.title));
    }
    Ok((to_value(&list)?, human))
}

/// 본문 파일을 UTF-8 로 읽는다. BOM 은 지우고, 잘못된 인코딩은 원인과 함께 거부한다.
fn read_body(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    if path.as_os_str() == "-" {
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(Error::internal)?;
    } else {
        bytes = std::fs::read(path).map_err(|e| {
            Error::new(Code::NotFound)
                .with_message("error.io")
                .arg("path", path.display().to_string())
                .with_source(e)
        })?;
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        Error::new(Code::Validation)
            .with_message("error.input_encoding")
            .arg("path", path.display().to_string())
            .with_hint("hint.input_encoding")
    })?;
    Ok(text.trim_start_matches('\u{feff}').to_owned())
}

async fn post_url(ctx: &Ctx, board_id: &str, slug: &str) -> Result<String> {
    let board = ctx
        .store
        .board_by_id(board_id)
        .await?
        .ok_or_else(|| Error::internal("글의 게시판이 없다"))?;
    Ok(urls::absolute(
        &ctx.site_url,
        &Target::Post {
            board: &board.slug,
            post: slug,
        },
    ))
}

// ---- board ----

async fn board(g: &Global, cmd: BoardCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        BoardCmd::List => {
            let list = boards::list(&ctx, &actor).await?;
            let mut human = format!("게시판 {}개", list.len());
            for b in &list {
                human.push_str(&format!("\n{}  {}  ({})", b.slug, b.name, b.kind));
            }
            Ok((to_value(&list)?, human))
        }
        BoardCmd::Create(args) => {
            let input = NewBoard {
                slug: args.slug,
                name: args.name,
                kind: args.kind,
                policy: None,
                sort_order: args.sort_order,
            };
            let (b, created) = if args.if_not_exists {
                let res = boards::ensure(&ctx, &actor, input).await?;
                (res.item, res.created)
            } else {
                (boards::create(&ctx, &actor, input).await?, true)
            };
            let mut data = to_value(&b)?;
            data["created"] = Value::Bool(created);
            let verb = if created {
                "만들었습니다"
            } else {
                "이미 있습니다"
            };
            Ok((data, format!("게시판 {verb}: {} ({})", b.name, b.slug)))
        }
        BoardCmd::Show { slug } => {
            let b = boards::show(&ctx, &actor, &slug).await?;
            let human = format!(
                "{} ({})  종류 {}  읽기 {}  쓰기 {}",
                b.name, b.slug, b.kind, b.policy.read, b.policy.write
            );
            Ok((to_value(&b)?, human))
        }
        BoardCmd::Update(args) => {
            let policy = if args.read.is_some() || args.write.is_some() {
                let current = boards::show(&ctx, &actor, &args.slug).await?.policy;
                Some(maru_core::model::BoardPolicy {
                    read: args.read.unwrap_or(current.read),
                    write: args.write.unwrap_or(current.write),
                    ..current
                })
            } else {
                None
            };
            let patch = boards::BoardPatch {
                name: args.name,
                kind: args.kind,
                policy,
                sort_order: args.sort_order,
            };
            let b = boards::update(&ctx, &actor, &args.slug, patch).await?;
            Ok((to_value(&b)?, format!("게시판을 고쳤습니다: {}", b.slug)))
        }
        BoardCmd::Delete { slug } => {
            let b = boards::delete(&ctx, &actor, &slug).await?;
            Ok((to_value(&b)?, format!("게시판을 지웠습니다: {}", b.slug)))
        }
    };
    db.close().await;
    res
}

// ---- token ----

async fn token(g: &Global, cmd: TokenCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        TokenCmd::Create(args) => token_create(&ctx, &actor, args).await,
    };
    db.close().await;
    res
}

async fn token_create(ctx: &Ctx, actor: &Actor, args: TokenCreateArgs) -> Out {
    let mut scopes = Vec::new();
    for s in &args.scopes {
        scopes.push(Scope::parse(s).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.scope_unknown")
                .arg("value", s.clone())
        })?);
    }
    let issued = tokens::create(
        ctx,
        actor,
        NewToken {
            name: args.name,
            scopes,
            board_ids: None,
            expires_at: args.expires_at,
        },
    )
    .await?;
    let human = format!(
        "토큰을 발급했습니다 ({}). 다시 볼 수 없으니 지금 보관하세요:\n  {}",
        issued
            .token
            .scopes
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        issued.secret
    );
    Ok((to_value(&issued)?, human))
}

// ---- page ----

async fn page(g: &Global, cmd: PageCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = page_inner(&ctx, &actor, cmd).await;
    db.close().await;
    res
}

async fn page_inner(ctx: &Ctx, actor: &Actor, cmd: PageCmd) -> Out {
    match cmd {
        PageCmd::New(args) => {
            let body_md = match (&args.body, &args.from) {
                (Some(b), _) => b.clone(),
                (None, Some(path)) => read_body(path)?,
                (None, None) => String::new(),
            };
            let input = pages::NewPage {
                slug: args.slug,
                title: args.title,
                body_md,
                layout: args.layout,
                sort_order: args.sort_order,
            };
            let (mut p, created) = if args.if_not_exists {
                let res = pages::ensure(ctx, actor, input).await?;
                (res.item, res.created)
            } else {
                (pages::create(ctx, actor, input).await?, true)
            };
            if args.publish {
                p = pages::publish(ctx, actor, &p.slug).await?;
            }
            let verb = if created {
                "만들었습니다"
            } else {
                "이미 있습니다"
            };
            let (mut data, mut human) = page_out(ctx, &p, verb)?;
            data["created"] = Value::Bool(created);
            if p.status == PageStatus::Draft {
                human.push_str(&format!("\n다음: maru page publish {}", p.slug));
            }
            Ok((data, human))
        }
        PageCmd::List { status } => {
            let status = match status.as_deref() {
                Some(s) => Some(PageStatus::parse(s).ok_or_else(|| {
                    Error::new(Code::Validation)
                        .with_message("error.post_status")
                        .arg("value", s)
                })?),
                None => None,
            };
            let list = pages::list(ctx, actor, status).await?;
            let mut human = format!("페이지 {}개", list.len());
            for p in &list {
                human.push_str(&format!(
                    "\n{}  [{}]  {}",
                    p.slug,
                    p.status.as_str(),
                    p.title
                ));
            }
            Ok((to_value(&list)?, human))
        }
        PageCmd::Show { slug } => {
            let p = pages::get(ctx, actor, &slug).await?;
            let human = format!("[{}] {}\n\n{}", p.status.as_str(), p.title, p.body_md);
            Ok((to_value(&p)?, human))
        }
        PageCmd::Edit(args) => {
            let body_md = match (&args.body, &args.from) {
                (Some(b), _) => Some(b.clone()),
                (None, Some(path)) => Some(read_body(path)?),
                (None, None) => None,
            };
            let patch = pages::PagePatch {
                title: args.title,
                slug: args.new_slug,
                body_md,
                layout: args.layout,
                sort_order: args.sort_order,
            };
            let p = pages::update(ctx, actor, &args.slug, patch).await?;
            page_out(ctx, &p, "고쳤습니다")
        }
        PageCmd::Publish { slug } => page_out(
            ctx,
            &pages::publish(ctx, actor, &slug).await?,
            "공개했습니다",
        ),
        PageCmd::Hide { slug } => {
            page_out(ctx, &pages::hide(ctx, actor, &slug).await?, "숨겼습니다")
        }
        PageCmd::Delete { slug } => {
            let p = pages::delete(ctx, actor, &slug).await?;
            let (data, human) = page_out(ctx, &p, "삭제했습니다")?;
            Ok((
                data,
                format!("{human}\n되돌리기: maru page restore {}", p.slug),
            ))
        }
        PageCmd::Restore { slug } => page_out(
            ctx,
            &pages::restore(ctx, actor, &slug).await?,
            "되돌렸습니다",
        ),
    }
}

fn page_out(ctx: &Ctx, p: &Page, verb: &str) -> Out {
    let url = urls::absolute(&ctx.site_url, &Target::Page { page: &p.slug });
    let mut data = to_value(p)?;
    data["url"] = Value::String(url.clone());
    Ok((
        data,
        format!("페이지 {verb}: {} [{}]\n{url}", p.title, p.status.as_str()),
    ))
}

// ---- menu ----

async fn menu(g: &Global, cmd: MenuCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        MenuCmd::Show { location } => match menus::get(&ctx, &location).await {
            Ok(items) => to_value(&items).map(|v| (v, menu_human(&location, &items))),
            Err(e) => Err(e),
        },
        MenuCmd::Set(args) => menu_set(&ctx, &actor, args).await,
    };
    db.close().await;
    res
}

async fn menu_set(ctx: &Ctx, actor: &Actor, args: MenuSetArgs) -> Out {
    let items: Vec<MenuItem> = if let Some(path) = &args.from {
        serde_json::from_str(&read_body(path)?).map_err(|e| {
            Error::new(Code::Validation)
                .with_message("error.json_body")
                .arg("detail", e.to_string())
        })?
    } else if args.clear {
        vec![]
    } else if args.items.is_empty() {
        return Err(Error::new(Code::Usage)
            .with_hint("hint.usage")
            .next(format!(
                "maru menu set {} --item \"글=board:blog\" --item \"소개=page:about\"",
                args.location
            )));
    } else {
        parse_menu_items(&args.items)?
    };
    let saved = menus::set(ctx, actor, &args.location, items).await?;
    let human = format!(
        "메뉴를 저장했습니다.\n{}",
        menu_human(&args.location, &saved)
    );
    Ok((to_value(&saved)?, human))
}

/// `라벨=링크`, `상위라벨>라벨=링크` 를 트리로 바꾼다. 링크 검사는 서비스가 한다.
fn parse_menu_items(specs: &[String]) -> Result<Vec<MenuItem>> {
    let mut out: Vec<MenuItem> = Vec::new();
    for spec in specs {
        let bad = || {
            Error::new(Code::Validation)
                .with_message("error.menu_item")
                .arg("value", spec.clone())
        };
        let (label_part, link) = spec.split_once('=').ok_or_else(bad)?;
        let link = link.trim();
        let link = if link == "home" {
            MenuLink::Home
        } else if let Some(slug) = link.strip_prefix("board:") {
            MenuLink::Board { slug: slug.into() }
        } else if let Some(slug) = link.strip_prefix("page:") {
            MenuLink::Page { slug: slug.into() }
        } else {
            MenuLink::Url { href: link.into() }
        };
        match label_part.split_once('>') {
            Some((parent, label)) => {
                let parent = parent.trim();
                let p = out
                    .iter_mut()
                    .rev()
                    .find(|i| i.label == parent)
                    .ok_or_else(bad)?;
                p.children.push(MenuItem {
                    label: label.trim().into(),
                    link,
                    children: vec![],
                });
            }
            None => out.push(MenuItem {
                label: label_part.trim().into(),
                link,
                children: vec![],
            }),
        }
    }
    Ok(out)
}

fn menu_link_text(l: &MenuLink) -> String {
    match l {
        MenuLink::Home => "home".into(),
        MenuLink::Board { slug } => format!("board:{slug}"),
        MenuLink::Page { slug } => format!("page:{slug}"),
        MenuLink::Url { href } => href.clone(),
    }
}

fn menu_human(location: &str, items: &[MenuItem]) -> String {
    let mut s = format!("{location} 메뉴 {}개", items.len());
    for i in items {
        s.push_str(&format!("\n- {} → {}", i.label, menu_link_text(&i.link)));
        for c in &i.children {
            s.push_str(&format!("\n  - {} → {}", c.label, menu_link_text(&c.link)));
        }
    }
    s
}

// ---- site ----

async fn site(g: &Global, cmd: SiteCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        SiteCmd::Show => settings::site(&ctx).await,
        SiteCmd::Set(args) => {
            let patch = settings::SettingsPatch {
                name: args.name,
                description: args.description,
                lang: args.lang,
                tz_offset_min: args.tz_offset,
                posts_per_page: args.posts_per_page,
            };
            settings::update(&ctx, &actor, patch).await
        }
    };
    db.close().await;
    let s = res?;
    let human = format!(
        "이름 {}\n설명 {}\n언어 {}  시간대 UTC{:+}분  페이지당 글 {}",
        s.name, s.description, s.lang, s.tz_offset_min, s.posts_per_page
    );
    Ok((to_value(&s)?, human))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_item_specs_build_a_tree() {
        let specs = [
            "글=board:blog".to_owned(),
            "글>소개=page:about".to_owned(),
            "깃허브=https://github.com".to_owned(),
        ];
        let tree = parse_menu_items(&specs).unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(
            tree[0].children[0].link,
            MenuLink::Page {
                slug: "about".into()
            }
        );
        assert_eq!(
            tree[1].link,
            MenuLink::Url {
                href: "https://github.com".into()
            }
        );
        assert!(parse_menu_items(&["라벨만".to_owned()]).is_err());
        assert!(parse_menu_items(&["없는상위>x=home".to_owned()]).is_err());
    }
}
