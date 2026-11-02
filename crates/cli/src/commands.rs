//! 명령 구현. 모든 명령은 `maru_core::services` 를 부르고 결과를 출력 계약으로 낸다.

use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use maru_core::actor::Scope;
use maru_core::config::{Config, DEFAULT_BIND};
use maru_core::model::{PageReq, PostStatus};
use maru_core::services::boards::{self, NewBoard};
use maru_core::services::posts::{self, NewPost};
use maru_core::services::tokens::{self, NewToken};
use maru_core::urls::{self, Target};
use maru_core::{Actor, Code, Ctx, Error, Result};
use maru_storage::{DataDir, Db};
use serde_json::{Value, json};

use crate::output;
use crate::{
    BoardCmd, Command, Global, InitArgs, PostCmd, PostListArgs, PostNewArgs, ServeArgs, TokenCmd,
    TokenCreateArgs,
};

pub async fn run(g: &Global, command: Command) -> ExitCode {
    let res = match command {
        Command::Init(args) => init(g, args).await,
        Command::Serve(args) => return serve(g, args).await,
        Command::Post(cmd) => post(g, cmd).await,
        Command::Board(cmd) => board(g, cmd).await,
        Command::Token(cmd) => token(g, cmd).await,
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
            let url = post_url(&ctx, &p.board_id, &p.slug).await?;
            let mut data = to_value(&p)?;
            data["url"] = Value::String(url.clone());
            Ok((data, format!("발행했습니다: {}\n{url}", p.title)))
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

async fn post_new(ctx: &Ctx, actor: &Actor, args: PostNewArgs) -> Out {
    let body_md = match (&args.body, &args.from) {
        (Some(b), _) => b.clone(),
        (None, Some(path)) => read_body(path)?,
        (None, None) => String::new(),
    };
    let mut p = posts::create_draft(
        ctx,
        actor,
        NewPost {
            board: args.board,
            title: args.title,
            slug: args.slug,
            body_md,
            excerpt: args.excerpt,
        },
    )
    .await?;
    if args.publish {
        p = posts::publish(ctx, actor, &p.id).await?;
    }
    let url = post_url(ctx, &p.board_id, &p.slug).await?;
    let mut data = to_value(&p)?;
    data["url"] = Value::String(url.clone());
    let verb = if args.publish {
        "발행"
    } else {
        "초안 저장"
    };
    let mut human = format!("{verb}: {} (id {})\n{url}", p.title, p.id);
    if !args.publish {
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
