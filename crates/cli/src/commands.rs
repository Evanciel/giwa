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
use maru_core::services::{
    anonymous_actor, auth, categories, comments, design, files, fonts, mail, media, members, menus,
    pages, presets, search, settings, tags, templates, views,
};
use maru_core::urls::{self, Target};
use maru_core::{Actor, Code, Ctx, Error, Result};
use maru_storage::{DataDir, Db};
use serde_json::{Value, json};

use crate::output;
use crate::{
    BoardCmd, CategoryCmd, Command, CommentCmd, DesignCmd, FileCmd, FontCmd, Global, InitArgs,
    LoginLinkArgs, MailCmd, MediaCmd, MemberCmd, MenuCmd, MenuSetArgs, PageCmd, PostCmd,
    PostListArgs, PostNewArgs, SearchArgs, SearchCmd, ServeArgs, SiteCmd, TagCmd, TemplateCmd,
    TokenCmd, TokenCreateArgs,
};

pub async fn run(g: &Global, command: Command) -> ExitCode {
    let res = match command {
        Command::Init(args) => init(g, args).await,
        Command::Serve(args) => return serve(g, args).await,
        Command::Doctor => return crate::doctor::run(g).await,
        Command::Post(cmd) => post(g, cmd).await,
        Command::Board(cmd) => board(g, cmd).await,
        Command::Token(cmd) => token(g, cmd).await,
        Command::Media(cmd) => media_cmd(g, cmd).await,
        Command::File(cmd) => file_cmd(g, cmd).await,
        Command::Template(cmd) => template_cmd(g, cmd).await,
        Command::Category(cmd) => category_cmd(g, cmd).await,
        Command::Tag(cmd) => tag_cmd(g, cmd).await,
        Command::Page(cmd) => page(g, cmd).await,
        Command::Menu(cmd) => menu(g, cmd).await,
        Command::Site(cmd) => site(g, cmd).await,
        Command::Design(cmd) => design_cmd(g, cmd).await,
        Command::Font(cmd) => font_cmd(g, cmd).await,
        Command::Search(args) => search_cmd(g, args).await,
        Command::Member(cmd) => member_cmd(g, cmd).await,
        Command::Comment(cmd) => comment_cmd(g, cmd).await,
        Command::Mail(cmd) => mail_cmd(g, cmd).await,
        Command::LoginLink(args) => login_link(g, args).await,
    };
    match res {
        Ok((data, _)) if g.quiet && !g.json => {
            for line in essentials(&data) {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Ok((data, human)) => output::success(data, g.json, |_| human),
        Err(e) => output::error(&e, g.json),
    }
}

type Out = Result<(Value, String)>;

/// `--quiet` 일 때도 꼭 내보낼 값: 다시 볼 수 없는 토큰 원문과 로그인 링크.
fn essentials(data: &Value) -> Vec<String> {
    [
        &data["secret"],
        &data["token"]["secret"],
        &data["login_link"]["url"],
        &data["url"],
    ]
    .iter()
    .filter_map(|v| v.as_str().map(str::to_owned))
    .collect()
}

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
    let preset =
        maru_themes::preset(&args.preset).map_err(|e| e.next("maru init --preset blog --yes"))?;
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
        blobs: Arc::new(maru_storage::FsBlobs::new(dir.media_dir())),
        site_url: config.site_url.clone(),
    };
    let local = Actor::local();
    let applied = presets::apply(&ctx, &local, &preset).await?;
    if let Some(name) = args.name {
        let patch = settings::SettingsPatch {
            name: Some(name),
            ..Default::default()
        };
        settings::update(&ctx, &local, patch).await?;
    }
    // 초기 토큰은 `init` 이름의 토큰을 한 번도 만든 적 없을 때만 발급한다.
    // 첫 실행이 중간에 실패했어도 다시 실행하면 받을 수 있고, 폐기한 토큰은 되살리지 않는다.
    let issued_before = ctx
        .store
        .list_tokens()
        .await?
        .iter()
        .any(|t| t.name == "init");
    let token = if !issued_before {
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
    // 회원이 없으면 운영자를 만들고 로그인 링크를 낸다. 이미 있으면 건드리지 않는다.
    let owner = if ctx.store.count_members().await? == 0 || args.owner_email.is_some() {
        let email = args.owner_email.as_deref().unwrap_or("owner@localhost");
        Some(auth::ensure_owner(&ctx, &local, email, &args.owner_name).await?)
    } else {
        None
    };
    let link = match &owner {
        Some(o) if o.created => {
            Some(auth::issue_login_link(&ctx, &local, Some(&o.item.email)).await?)
        }
        _ => None,
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
        "preset": preset.id,
        "applied": to_value(&applied)?,
        "token": token.as_ref().map(|t| json!({
            "id": t.token.id,
            "name": t.token.name,
            "scopes": t.token.scopes,
            "secret": t.secret,
        })),
        "owner": owner.as_ref().map(|o| json!({ "email": o.item.email, "created": o.created })),
        "login_link": link.as_ref().map(|l| json!({ "url": l.url, "expires_at": l.expires_at })),
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
    if let Some(l) = &link {
        human.push_str(&format!(
            "관리 화면 로그인 링크 (10분, 1회용). 서버를 켠 뒤 여세요:
  {}
",
            l.url
        ));
    }
    human.push_str(&format!("다음: {serve_cmd}"));
    Ok((data, human))
}

// ---- login-link ----

async fn login_link(g: &Global, args: LoginLinkArgs) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = auth::issue_login_link(&ctx, &actor, args.email.as_deref()).await;
    db.close().await;
    let l = res?;
    let data = json!({
        "url": l.url,
        "expires_at": l.expires_at,
        "email": l.member.email,
    });
    let human = format!(
        "로그인 링크 ({}, 10분 동안 한 번만 쓸 수 있습니다):
  {}",
        l.member.email, l.url
    );
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
    // 편지는 지금 파일로만 남긴다 (SMTP 는 ADR-0051 승인 뒤).
    let mailer = maru_storage::mailer::FileMailer::new(dir.mail_dir(), &config.site_url);
    let mail_dir = mailer.dir().display().to_string();
    let state = maru_web::AppState::new(ctx, theme).with_mailer(Arc::new(mailer));
    let started = json!({
        "url": url,
        "bind": local.to_string(),
        "site_url": config.site_url,
        "mail": { "transport": "file", "dir": mail_dir },
    });
    let human = format!(
        "서버를 시작했습니다: {url}\n편지(가입 인증·비밀번호 찾기)는 파일로만 남습니다: {mail_dir}\n멈추려면 Ctrl+C"
    );
    output::success(started, g.json, |_| human);
    if !g.quiet {
        tracing::info!(%url, "serving");
    }

    // 종료 신호를 받으면 새 연결을 막고 진행 중인 요청을 기다린다. 너무 오래 걸리면 강제로 멈춘다.
    let (stopping, stopped) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        shutdown_signal().await;
        let _ = stopping.send(());
    };
    let leftover = state.clone();
    let serve = maru_web::serve(state, listener, shutdown);
    tokio::pin!(serve);
    let res = tokio::select! {
        r = &mut serve => r,
        () = async {
            if stopped.await.is_ok() {
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            tracing::warn!("진행 중인 요청이 끝나지 않아 강제로 멈춥니다");
            maru_web::flush_views(&leftover).await;
            Ok(())
        }
    };
    db.close().await;
    if res.is_ok() && !g.json && !g.quiet {
        println!("서버를 멈췄습니다.");
    }
    res
}

const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Ctrl+C, 그리고 운영체제의 종료 요청(Unix SIGTERM, Windows 창 닫기·로그오프·시스템 종료).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        if let (Ok(mut close), Ok(mut shut)) = (windows::ctrl_close(), windows::ctrl_shutdown()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = close.recv() => {}
                _ = shut.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
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
                cover: args.cover,
                scheduled_at: schedule_arg(&ctx, args.schedule).await?,
                category: args.category,
                tags: if args.clear_tags {
                    Some(vec![])
                } else {
                    (!args.tags.is_empty()).then_some(args.tags)
                },
                notice: args.notice,
                secret: args.secret,
            };
            let p = posts::update(&ctx, &actor, &args.id, patch).await?;
            post_out(&ctx, &p, "고쳤습니다").await
        }
        PostCmd::List(args) => post_list(&ctx, &actor, args).await,
        PostCmd::Revisions { id } => {
            let mut list = posts::revisions(&ctx, &actor, &id).await?;
            list.reverse();
            let mut human = format!("리비전 {}개 (새것부터)", list.len());
            for r in &list {
                human.push_str(&format!(
                    "\n{}  {}  {}",
                    r.id,
                    r.created_at,
                    r.actor_type.as_str()
                ));
            }
            Ok((to_value(&list)?, human))
        }
        PostCmd::Diff { id, revision } => {
            let c = posts::compare(&ctx, &actor, &id, &revision).await?;
            let mut human = format!(
                "{} → 지금: +{} −{}{}",
                c.revision.created_at,
                c.body.added,
                c.body.removed,
                if c.title_changed {
                    format!("\n제목: {} → {}", c.revision.title, c.current_title)
                } else {
                    String::new()
                }
            );
            for l in &c.body.lines {
                let mark = match l.op {
                    maru_core::diff::Op::Same => ' ',
                    maru_core::diff::Op::Add => '+',
                    maru_core::diff::Op::Del => '-',
                };
                human.push_str(&format!("\n{mark} {}", l.text));
            }
            Ok((to_value(&c)?, human))
        }
        PostCmd::Revert { id, revision } => {
            let p = posts::revert(&ctx, &actor, &id, &revision).await?;
            post_out(&ctx, &p, "리비전으로 되돌렸습니다").await
        }
        PostCmd::Move {
            id,
            board,
            category,
        } => {
            let p = posts::move_to(&ctx, &actor, &id, posts::MoveInput { board, category }).await?;
            post_out(&ctx, &p, "옮겼습니다").await
        }
        PostCmd::Copy { id, board } => {
            let p = posts::copy_to(&ctx, &actor, &id, posts::CopyInput { board }).await?;
            post_out(&ctx, &p, "초안으로 복사했습니다").await
        }
        PostCmd::Bulk(args) => {
            let action = match args.action.as_str() {
                "publish" => posts::BulkAction::Publish,
                "hide" => posts::BulkAction::Hide,
                "delete" => posts::BulkAction::Delete,
                "restore" => posts::BulkAction::Restore,
                "move" => posts::BulkAction::Move {
                    board: args.board.ok_or_else(|| {
                        Error::new(Code::Validation)
                            .with_message("error.bulk_board")
                            .next("maru post bulk move --board <게시판> --id <글>")
                    })?,
                    category: args.category,
                },
                other => {
                    return Err(Error::new(Code::Validation)
                        .with_message("error.bulk_action")
                        .arg("value", other.to_owned()));
                }
            };
            let items = posts::bulk(
                &ctx,
                &actor,
                posts::BulkInput {
                    ids: args.ids,
                    action,
                },
            )
            .await?;
            let ok = items.iter().filter(|i| i.ok).count();
            let mut human = format!("{}개 중 {ok}개 처리", items.len());
            for i in items.iter().filter(|i| !i.ok) {
                if let Some(e) = &i.error {
                    human.push_str(&format!("\n실패 {}: {}", i.id, e.message));
                }
            }
            Ok((to_value(&items)?, human))
        }
        PostCmd::Show { id } => {
            let p = posts::get(&ctx, &actor, &id).await?;
            let human = format!(
                "[{}] {}  조회 {}\n\n{}",
                p.status.as_str(),
                p.title,
                p.view_count,
                p.body_md
            );
            Ok((to_value(&p)?, human))
        }
        PostCmd::Nearby { board, slug } => {
            let (board, p) = posts::public_post(&ctx, &actor, &board, &slug).await?;
            let near = posts::nearby(&ctx, &actor, &board, &p).await?;
            let line = |label: &str, q: &Option<maru_core::model::Post>| match q {
                Some(q) => format!("{label}: {} ({})", q.title, q.slug),
                None => format!("{label}: 없음"),
            };
            let mut human = format!(
                "{}\n{}",
                line("이전 글", &near.prev),
                line("다음 글", &near.next)
            );
            if near.related.is_empty() {
                human.push_str("\n관련 글: 없음");
            } else {
                human.push_str("\n관련 글:");
                for r in &near.related {
                    human.push_str(&format!("\n  - {} ({})", r.title, r.slug));
                }
            }
            Ok((to_value(&near)?, human))
        }
        PostCmd::Views { id, set } => {
            let p = views::set(&ctx, &actor, &id, set).await?;
            post_out(
                &ctx,
                &p,
                &format!("조회수를 {}(으)로 바꿨습니다", p.view_count),
            )
            .await
        }
        PostCmd::CheckSlug(args) => {
            let input = posts::SlugCheckInput {
                board: args.board,
                slug: args.slug,
                title: args.title,
                post_id: args.post,
            };
            let out = posts::check_slug(&ctx, &actor, input).await?;
            let human = match (&out.available, &out.suggestion) {
                (true, _) => format!("쓸 수 있습니다: {}", out.path),
                (false, Some(next)) => format!(
                    "이미 쓰는 주소입니다: {} (비어 있는 주소: {next})",
                    out.slug
                ),
                (false, None) => format!("이미 쓰는 주소입니다: {}", out.slug),
            };
            Ok((to_value(&out)?, human))
        }
        PostCmd::Preview { from } => {
            let input = posts::PreviewInput {
                body_md: read_body(&from)?,
            };
            let out = posts::preview(&ctx, &actor, input)?;
            Ok((to_value(&out)?, out.html))
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
    let mut human = format!("{verb}: {} [{}]\n{url}", p.title, p.status.as_str());
    if let Some(at) = &p.scheduled_at {
        human.push_str(&format!("\n예약 발행: {at} (UTC)"));
    }
    Ok((data, human))
}

/// `--schedule` 값을 서비스가 받는 RFC3339 로. 빈 값은 그대로(예약 취소).
async fn schedule_arg(ctx: &Ctx, value: Option<String>) -> Result<Option<String>> {
    let Some(v) = value else {
        return Ok(None);
    };
    if v.trim().is_empty() {
        return Ok(Some(String::new()));
    }
    let tz = settings::site(ctx).await?.tz_offset_min;
    maru_core::clock::from_local_input(&v, tz)
        .map(Some)
        .ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.timestamp")
                .arg("value", v)
        })
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
        cover: args.cover,
        category: args.category,
        tags: args.tags,
        notice: args.notice,
        secret: args.secret,
    };
    let schedule = schedule_arg(ctx, args.schedule).await?;
    let (mut p, created) = if args.if_not_exists {
        let res = posts::ensure_draft(ctx, actor, input).await?;
        (res.item, res.created)
    } else {
        (posts::create_draft(ctx, actor, input).await?, true)
    };
    if args.publish {
        p = posts::publish(ctx, actor, &p.id).await?;
    }
    if let Some(at) = schedule {
        let patch = posts::PostPatch {
            scheduled_at: Some(at),
            ..Default::default()
        };
        p = posts::update(ctx, actor, &p.id, patch).await?;
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
    if let Some(at) = &p.scheduled_at {
        human.push_str(&format!("\n예약 발행: {at} (UTC)"));
    } else if !args.publish && created {
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
        args.q.as_deref(),
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
                layout: args.layout,
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
                "{} ({})  종류 {}  형태 {}  읽기 {}  쓰기 {} (승인 {})  댓글 {} (승인 {})",
                b.name,
                b.slug,
                b.kind,
                b.effective_layout(),
                b.policy.read,
                b.policy.write,
                b.policy.write_approval.as_str(),
                b.policy.comment,
                b.policy.comment_approval.as_str()
            );
            Ok((to_value(&b)?, human))
        }
        BoardCmd::Update(args) => {
            let policy = if args.read.is_some()
                || args.write.is_some()
                || args.comment.is_some()
                || args.comment_approval.is_some()
                || args.write_approval.is_some()
            {
                let current = boards::show(&ctx, &actor, &args.slug).await?.policy;
                let approval = |v: Option<&str>, now| match v {
                    None => Ok(now),
                    Some(s) => maru_core::model::Approval::parse(s).ok_or_else(|| {
                        Error::new(Code::Validation)
                            .with_message("error.comment_policy")
                            .arg("value", s)
                            .arg("allowed", "off, guest, all")
                    }),
                };
                let comment_approval =
                    approval(args.comment_approval.as_deref(), current.comment_approval)?;
                let write_approval =
                    approval(args.write_approval.as_deref(), current.write_approval)?;
                Some(maru_core::model::BoardPolicy {
                    read: args.read.unwrap_or(current.read),
                    write: args.write.unwrap_or(current.write),
                    comment: args.comment.unwrap_or(current.comment),
                    comment_approval,
                    write_approval,
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
                layout: args.layout,
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
        TokenCmd::List => token_list(&ctx, &actor).await,
        TokenCmd::Revoke { id } => {
            let t = tokens::revoke(&ctx, &actor, &id).await;
            t.and_then(|t| {
                let human = format!("토큰을 폐기했습니다: {} ({})", t.name, t.id);
                Ok((to_value(&t)?, human))
            })
        }
    };
    db.close().await;
    res
}

async fn token_list(ctx: &Ctx, actor: &Actor) -> Out {
    let list = tokens::list(ctx, actor).await?;
    let now = maru_core::clock::now();
    let mut human = String::new();
    for t in &list {
        let state = if t.revoked_at.is_some() {
            "폐기됨"
        } else if t.expires_at.as_deref().is_some_and(|e| e <= now.as_str()) {
            "만료됨"
        } else {
            "사용 중"
        };
        let scopes: Vec<&str> = t.scopes.iter().map(|s| s.as_str()).collect();
        human.push_str(&format!(
            "{}  {}  [{}]  {}  마지막 사용: {}
",
            t.id,
            t.name,
            scopes.join(", "),
            state,
            t.last_used_at.as_deref().unwrap_or("-")
        ));
    }
    if list.is_empty() {
        human.push_str(
            "토큰이 없습니다.
",
        );
    }
    Ok((
        json!({ "tokens": to_value(&list)? }),
        human.trim_end().to_owned(),
    ))
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
                naver_verification: args.naver_verification,
                google_verification: args.google_verification,
                toc_min_headings: args.toc_min_headings,
                show_views: args.show_views,
                show_search: args.show_search,
                signup: args.signup,
                signup_verify: args.signup_verify,
                captcha: args.captcha,
            };
            settings::update(&ctx, &actor, patch).await
        }
    };
    db.close().await;
    let s = res?;
    let toc = match s.toc_min_headings {
        0 => "끔".to_owned(),
        n => format!("제목 {n}개 이상"),
    };
    let human = format!(
        "이름 {}\n설명 {}\n언어 {}  시간대 UTC{:+}분  페이지당 글 {}  목차 {toc}  조회수 {}  검색 칸 {}\n회원 가입 {}  메일 인증 {}  확인 질문 {}",
        s.name,
        s.description,
        s.lang,
        s.tz_offset_min,
        s.posts_per_page,
        if s.show_views { "보임" } else { "숨김" },
        if s.show_search { "보임" } else { "숨김" },
        match s.signup {
            maru_core::model::SignupMode::Off => "받지 않음",
            maru_core::model::SignupMode::Open => "누구나",
        },
        if s.signup_verify { "필요" } else { "없음" },
        match s.captcha {
            maru_core::model::CaptchaMode::Off => "끔",
            maru_core::model::CaptchaMode::Guest => "비회원·가입",
        }
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

// ---- design ----

async fn design_cmd(g: &Global, cmd: DesignCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = design_inner(&ctx, &actor, cmd).await;
    db.close().await;
    res
}

async fn design_inner(ctx: &Ctx, actor: &Actor, cmd: DesignCmd) -> Out {
    let (tokens, verb) = match cmd {
        DesignCmd::Show { css: true } => {
            let css = design::tokens(ctx).await?.to_css(&fonts::list(ctx).await?);
            return Ok((json!({ "css": css }), css));
        }
        DesignCmd::Show { css: false } => (design::tokens(ctx).await?, None),
        DesignCmd::Reset => (
            design::reset(ctx, actor).await?,
            Some("기본값으로 되돌렸습니다"),
        ),
        DesignCmd::Set(args) => {
            let mut patch = match &args.from {
                Some(path) => serde_json::from_str(&read_body(path)?).map_err(|e| {
                    Error::new(Code::Validation)
                        .with_message("error.json_body")
                        .arg("detail", e.to_string())
                })?,
                None => json!({}),
            };
            for spec in &args.sets {
                set_path(&mut patch, spec)?;
            }
            if args.sets.is_empty() && args.from.is_none() {
                return Err(Error::new(Code::Usage)
                    .with_hint("hint.usage")
                    .next("maru design set --set color.primary=#2f5d50"));
            }
            (
                design::update(ctx, actor, patch).await?,
                Some("저장했습니다"),
            )
        }
    };
    let c = &tokens.color;
    let mut human = format!(
        "색  배경 {}  글자 {}  강조 {}  보조 {}\n글꼴 본문 {}  제목 {}  코드 {}\n크기 본문 {}px  배율 {}  최대 폭 {}px",
        c.bg,
        c.fg,
        c.primary,
        c.muted,
        tokens.font.body,
        tokens.font.heading,
        tokens.font.mono,
        tokens.type_scale.base,
        tokens.type_scale.ratio,
        tokens.layout.max_width
    );
    if let Some(v) = verb {
        human = format!("디자인 토큰을 {v}.\n{human}");
    }
    Ok((to_value(&tokens)?, human))
}

/// `color.primary=#2f5d50` 을 부분 JSON 에 넣는다. 숫자로 읽히면 숫자로 넣는다.
fn set_path(patch: &mut Value, spec: &str) -> Result<()> {
    let bad = || {
        Error::new(Code::Validation)
            .with_message("error.token_value")
            .arg("field", spec.to_owned())
            .arg("value", "")
            .next("maru design set --set color.primary=#2f5d50")
    };
    let (path, raw) = spec.split_once('=').ok_or_else(bad)?;
    let value = match serde_json::from_str::<Value>(raw.trim()) {
        Ok(n @ Value::Number(_)) => n,
        _ => Value::String(raw.trim().to_owned()),
    };
    let mut slot = patch;
    let keys: Vec<&str> = path.trim().split('.').collect();
    for (i, key) in keys.iter().enumerate() {
        if key.is_empty() {
            return Err(bad());
        }
        let obj = slot.as_object_mut().ok_or_else(bad)?;
        if i + 1 == keys.len() {
            obj.insert((*key).to_owned(), value);
            return Ok(());
        }
        slot = obj.entry((*key).to_owned()).or_insert_with(|| json!({}));
    }
    Err(bad())
}

#[cfg(test)]
mod design_tests {
    use super::*;

    #[test]
    fn set_path_builds_nested_patch() {
        let mut p = json!({});
        set_path(&mut p, "color.primary=#123456").unwrap();
        set_path(&mut p, "type_scale.base=18").unwrap();
        assert_eq!(
            p,
            json!({ "color": { "primary": "#123456" }, "type_scale": { "base": 18 } })
        );
        assert!(set_path(&mut p, "nonsense").is_err());
        assert!(set_path(&mut p, "color..x=1").is_err());
    }
}

// ---- media ----

async fn media_cmd(g: &Global, cmd: MediaCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        MediaCmd::Upload(args) => {
            let bytes = std::fs::read(&args.file).map_err(|e| {
                Error::new(Code::NotFound)
                    .with_message("error.io")
                    .arg("path", args.file.display().to_string())
                    .with_source(e)
            })?;
            let filename = args
                .file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let input = media::Upload {
                filename,
                bytes,
                alt_text: args.alt,
            };
            let m = media::upload(&ctx, &actor, input).await?;
            media_out(&m, "올렸습니다")
        }
        MediaCmd::List { page, per_page } => {
            let list = media::list(&ctx, &actor, PageReq::new(page, per_page)).await?;
            let mut human = format!("미디어 {}개", list.total);
            for m in &list.items {
                human.push_str(&format!(
                    "\n{}  {}x{}  {}  {}",
                    m.id,
                    m.width,
                    m.height,
                    m.url,
                    m.alt_text.as_deref().unwrap_or("(대체 텍스트 없음)")
                ));
            }
            Ok((to_value(&list)?, human))
        }
        MediaCmd::Show { id } => {
            let m = media::get(&ctx, &actor, &id).await?;
            media_out(&m, "미디어")
        }
        MediaCmd::Alt { id, text } => {
            let patch = media::MediaPatch {
                alt_text: Some(text),
            };
            let m = media::update(&ctx, &actor, &id, patch).await?;
            media_out(&m, "대체 텍스트를 바꿨습니다")
        }
        MediaCmd::Delete { id } => {
            let m = media::delete(&ctx, &actor, &id).await?;
            media_out(&m, "지웠습니다")
        }
    };
    db.close().await;
    res
}

async fn file_cmd(g: &Global, cmd: FileCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        FileCmd::Upload { file } => {
            let bytes = std::fs::read(&file).map_err(|e| {
                Error::new(Code::NotFound)
                    .with_message("error.io")
                    .arg("path", file.display().to_string())
                    .with_source(e)
            })?;
            let filename = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let f = files::upload(&ctx, &actor, files::Upload { filename, bytes }).await;
            f.and_then(|f| file_out(&f, "올렸습니다"))
        }
        FileCmd::List { page, per_page } => {
            let list = files::list(&ctx, &actor, PageReq::new(page, per_page)).await?;
            let mut human = format!("첨부 파일 {}개", list.total);
            for f in &list.items {
                human.push_str(&format!(
                    "\n{}  {}KB  {}  {}",
                    f.id,
                    f.size / 1024,
                    f.url,
                    f.filename
                ));
            }
            Ok((to_value(&list)?, human))
        }
        FileCmd::Delete { id } => {
            let f = files::delete(&ctx, &actor, &id).await?;
            file_out(&f, "지웠습니다")
        }
    };
    db.close().await;
    res
}

// ---- font ----

async fn font_cmd(g: &Global, cmd: FontCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = font_inner(g, &ctx, &actor, cmd).await;
    db.close().await;
    res
}

async fn font_inner(g: &Global, ctx: &Ctx, actor: &Actor, cmd: FontCmd) -> Out {
    match cmd {
        FontCmd::List => {
            let list = fonts::list(ctx).await?;
            let mut human = format!("올린 서체 {}개", list.len());
            for f in &list {
                let weights: Vec<String> = f
                    .files
                    .iter()
                    .map(|x| format!("{}{}", x.weight, if x.italic { "i" } else { "" }))
                    .collect();
                human.push_str(&format!(
                    "\n{}  {}  굵기 [{}]  {}",
                    f.slug,
                    f.name,
                    weights.join(" "),
                    f.license
                ));
            }
            Ok((to_value(&list)?, human))
        }
        FontCmd::Show { slug } => font_out(&fonts::get(ctx, &slug).await?, "서체", &[]),
        FontCmd::Add(args) => {
            let bytes = read_file(&args.file)?;
            let license_text = args.license_file.as_deref().map(read_body).transpose()?;
            let input = fonts::NewFont {
                name: args.name,
                slug: args.slug,
                fallback: args.fallback,
                license: args.license,
                license_text,
                rights_confirmed: g.yes,
            };
            let upload = fonts::FontUpload {
                weight: args.weight,
                italic: args.italic,
                bytes,
            };
            let up = fonts::add_with_file(ctx, actor, input, upload).await?;
            font_out(&up.font, "올렸습니다", &up.warnings)
        }
        FontCmd::Update(args) => {
            let license_text = if args.no_license_file {
                Some(String::new())
            } else {
                args.license_file.as_deref().map(read_body).transpose()?
            };
            let patch = fonts::FontPatch {
                name: args.name,
                fallback: args.fallback,
                license: args.license,
                license_text,
            };
            font_out(
                &fonts::update(ctx, actor, &args.slug, patch).await?,
                "고쳤습니다",
                &[],
            )
        }
        FontCmd::Remove {
            slug,
            weight: Some(weight),
            italic,
        } => font_out(
            &fonts::remove_file(ctx, actor, &slug, weight, italic).await?,
            "파일을 뗐습니다",
            &[],
        ),
        FontCmd::Remove { slug, .. } => {
            let r = fonts::remove(ctx, actor, &slug).await?;
            let mut human = format!("지웠습니다: {} ({})", r.font.name, r.font.slug);
            if r.used_in_posts > 0 {
                human.push_str(&format!(
                    "\n이 서체를 쓴 글·페이지 {}개는 기본 서체로 보입니다.",
                    r.used_in_posts
                ));
            }
            Ok((to_value(&r)?, human))
        }
    }
}

/// 사람용 출력에는 글 안에 쓰는 법과 사이트 본문 서체로 쓰는 명령을 같이 낸다.
fn font_out(f: &maru_core::model::Font, verb: &str, warnings: &[String]) -> Out {
    let mut human = format!("{verb}: {} ({})  라이선스 {}", f.name, f.slug, f.license);
    for x in &f.files {
        human.push_str(&format!(
            "\n  굵기 {}{}  {}  {}KB  {}",
            x.weight,
            if x.italic { " 기울임" } else { "" },
            x.format,
            x.size / 1024,
            x.url
        ));
    }
    human.push_str(&format!(
        "\n글에 쓰기: :font[글자]{{{0}}}\n사이트 본문 서체로: maru design set --set font.body={0}",
        f.slug
    ));
    for w in warnings {
        human.push_str(&format!("\n주의: {w}"));
    }
    let mut data = to_value(f)?;
    if !warnings.is_empty()
        && let Some(obj) = data.as_object_mut()
    {
        obj.insert("warnings".into(), json!(warnings));
    }
    Ok((data, human))
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| {
        Error::new(Code::NotFound)
            .with_message("error.io")
            .arg("path", path.display().to_string())
            .with_source(e)
    })
}

async fn template_cmd(g: &Global, cmd: TemplateCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        TemplateCmd::List => {
            let list = templates::list(&ctx, &actor).await?;
            let mut human = format!("글 틀 {}개", list.len());
            for t in &list {
                let kind = if t.builtin { "기본" } else { "저장" };
                human.push_str(&format!("\n{}  [{kind}] {}", t.id, t.name));
            }
            Ok((to_value(&list)?, human))
        }
        TemplateCmd::Save { name, body, from } => {
            let body_md = match (body, from) {
                (Some(b), _) => b,
                (None, Some(path)) => read_body(&path)?,
                (None, None) => String::new(),
            };
            let t =
                templates::save(&ctx, &actor, templates::SaveTemplate { name, body_md }).await?;
            let human = format!("틀을 저장했습니다: {} (id {})", t.name, t.id);
            Ok((to_value(&t)?, human))
        }
        TemplateCmd::Delete { id } => {
            let t = templates::delete(&ctx, &actor, &id).await?;
            let human = format!("틀을 지웠습니다: {}", t.name);
            Ok((to_value(&t)?, human))
        }
    };
    db.close().await;
    res
}

async fn category_cmd(g: &Global, cmd: CategoryCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let one = |c: &maru_core::model::Category, verb: &str| -> Out {
        Ok((
            to_value(c)?,
            format!("{verb}: {} (주소 {})", c.name, c.slug),
        ))
    };
    let res = match cmd {
        CategoryCmd::List { board } => {
            let list = categories::list(&ctx, &actor, &board).await?;
            let mut human = format!("{board} 게시판 분류 {}개", list.len());
            for c in &list {
                human.push_str(&format!("\n{:>3}  {}  ({})", c.sort_order, c.name, c.slug));
            }
            if list.is_empty() {
                human.push_str(&format!(
                    "\n만들기: maru category add --board {board} <이름>"
                ));
            }
            Ok((to_value(&list)?, human))
        }
        CategoryCmd::Add { board, name, slug } => {
            let input = categories::NewCategory { board, name, slug };
            let c = categories::create(&ctx, &actor, input).await?;
            one(&c, "분류")
        }
        CategoryCmd::Rename {
            board,
            slug,
            name,
            new_slug,
        } => {
            let patch = categories::CategoryPatch {
                name: Some(name),
                slug: new_slug,
                sort_order: None,
            };
            let c = categories::update(&ctx, &actor, &board, &slug, patch).await?;
            one(&c, "이름을 바꿨습니다")
        }
        CategoryCmd::Move { board, slug, order } => {
            let patch = categories::CategoryPatch {
                sort_order: Some(order),
                ..Default::default()
            };
            let c = categories::update(&ctx, &actor, &board, &slug, patch).await?;
            one(&c, "순서를 바꿨습니다")
        }
        CategoryCmd::Remove { board, slug } => {
            let c = categories::delete(&ctx, &actor, &board, &slug).await?;
            one(&c, "지웠습니다")
        }
    };
    db.close().await;
    res
}

/// 검색은 손님 기준(공개 화면과 같은 결과). 초안·비밀글까지 찾으려면 `maru post list --q`.
async fn search_cmd(g: &Global, args: SearchArgs) -> Out {
    if let Some(SearchCmd::Reindex) = args.cmd {
        let (ctx, _config, db) = open(g).await?;
        let actor = actor(&ctx).await?;
        let res = search::reindex(&ctx, &actor).await;
        db.close().await;
        let done = res?;
        let human = format!(
            "검색 색인을 다시 만들었습니다: 글 {}개, 페이지 {}개",
            done.posts, done.pages
        );
        return Ok((to_value(&done)?, human));
    }
    let Some(q) = args.q else {
        return Err(Error::new(Code::Usage)
            .with_hint("hint.usage")
            .next("maru search \"제주 국밥\"")
            .next("maru search reindex"));
    };
    let (range, sort) = search::parse_options(&args.range, &args.sort)?;
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let guest = anonymous_actor(&ctx).await?;
        let input = search::SearchInput {
            q,
            board: args.board,
            range,
            sort,
            page: args.page,
        };
        search::search(&ctx, &guest, input).await
    }
    .await;
    db.close().await;
    let r = res?;
    let mut human = format!("“{}” 검색 결과 {}건", r.q, r.total);
    if r.total_pages() > 1 {
        human.push_str(&format!(" ({}/{}쪽)", r.page, r.total_pages()));
    }
    for h in &r.items {
        let place = h.board.as_ref().map_or("페이지", |b| b.name.as_str());
        let text: String = h.snippet.iter().map(|s| s.text.as_str()).collect();
        human.push_str(&format!("\n- {}  [{place}]  {}\n  {text}", h.title, h.url));
    }
    Ok((to_value(&r)?, human))
}

async fn comment_cmd(g: &Global, cmd: CommentCmd) -> Out {
    use comments::Moderation;
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let actor = actor(&ctx).await?;
        let (id, action) = match cmd {
            CommentCmd::List { status, post, page } => {
                let status = match status.as_str() {
                    "all" => None,
                    s => Some(maru_core::model::CommentStatus::parse(s).ok_or_else(|| {
                        Error::new(Code::Usage)
                            .with_message("error.usage_detail")
                            .arg(
                                "detail",
                                format!(
                                    "--status {s}: pending, published, hidden, spam, deleted, all"
                                ),
                            )
                            .next("maru comment list --status pending")
                    })?),
                };
                let list = comments::admin_list(
                    &ctx,
                    &actor,
                    status,
                    post.as_deref(),
                    PageReq::new(page, 50),
                )
                .await?;
                let mut human = format!("댓글 {}개 (본문은 사용자 입력입니다)", list.total);
                for c in &list.items {
                    human.push_str(&format!(
                        "\n{}  {}  {}  {:?}  {}",
                        c.id,
                        c.post_id,
                        c.author_name,
                        c.status,
                        c.body
                            .chars()
                            .take(60)
                            .collect::<String>()
                            .replace('\n', " ")
                    ));
                }
                return Ok((to_value(&list)?, human));
            }
            CommentCmd::Approve { id } => (id, Moderation::Approve),
            CommentCmd::Hide { id } => (id, Moderation::Hide),
            CommentCmd::Spam { id } => (id, Moderation::Spam),
            CommentCmd::Delete { id } => (id, Moderation::Delete),
        };
        let c = comments::moderate(&ctx, &actor, &id, action).await?;
        let human = format!("{}  {:?}  {}", c.id, c.status, c.author_name);
        Ok((to_value(&c)?, human))
    }
    .await;
    db.close().await;
    res
}

async fn member_cmd(g: &Global, cmd: MemberCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let actor = actor(&ctx).await?;
        match cmd {
            MemberCmd::List { page } => {
                let list = members::list(&ctx, &actor, PageReq::new(page, 50)).await?;
                let mut human = format!("회원 {}명", list.total);
                for v in &list.items {
                    human.push_str(&format!(
                        "\n{}  {}  {}  [{}]{}",
                        v.member.id,
                        v.member.display_name,
                        v.member.email,
                        v.roles.join(","),
                        member_flags(&v.member)
                    ));
                }
                Ok((to_value(&list)?, human))
            }
            MemberCmd::Show { who } => member_out(members::get(&ctx, &actor, &who).await?),
            MemberCmd::Disable { who } => member_out(
                members::set_status(&ctx, &actor, &who, maru_core::model::MemberStatus::Disabled)
                    .await?,
            ),
            MemberCmd::Enable { who } => member_out(
                members::set_status(&ctx, &actor, &who, maru_core::model::MemberStatus::Active)
                    .await?,
            ),
            MemberCmd::Role { who, role } => {
                member_out(members::set_role(&ctx, &actor, &who, &role).await?)
            }
        }
    }
    .await;
    db.close().await;
    res
}

fn member_flags(m: &maru_core::model::Member) -> String {
    let mut f = String::new();
    if m.withdrawn_at.is_some() {
        f.push_str("  탈퇴");
    } else if m.status == maru_core::model::MemberStatus::Disabled {
        f.push_str("  비활성");
    }
    if m.email_verified_at.is_none() {
        f.push_str("  메일 인증 전");
    }
    f
}

fn member_out(v: members::MemberView) -> Out {
    let human = format!(
        "{}  {}  {}  [{}]{}",
        v.member.id,
        v.member.display_name,
        v.member.email,
        v.roles.join(","),
        member_flags(&v.member)
    );
    Ok((to_value(&v)?, human))
}

async fn mail_cmd(g: &Global, cmd: MailCmd) -> Out {
    let dir = DataDir::new(&g.data);
    let (ctx, config, db) = open(g).await?;
    let res = async {
        let actor = actor(&ctx).await?;
        match cmd {
            MailCmd::List { status, limit } => {
                let status = match status.as_deref() {
                    None => None,
                    Some(s) => Some(maru_core::model::MailStatus::parse(s).ok_or_else(|| {
                        Error::new(Code::Usage)
                            .with_message("error.usage_detail")
                            .arg("detail", format!("--status {s}: queued, sent, failed"))
                            .next("maru mail list --status queued")
                    })?),
                };
                let list = mail::list(&ctx, &actor, status, limit).await?;
                let mut human = format!("편지 {}통", list.len());
                for m in &list {
                    human.push_str(&format!(
                        "\n{}  {:?}  {}  {}",
                        m.id, m.status, m.to_addr, m.subject
                    ));
                }
                Ok((to_value(&list)?, human))
            }
            MailCmd::Show { id } => {
                let m = mail::get(&ctx, &actor, &id).await?;
                let human = format!(
                    "받는 사람 {}\n제목 {}\n상태 {:?} (시도 {}번)\n\n{}",
                    m.to_addr, m.subject, m.status, m.attempts, m.body
                );
                Ok((to_value(&m)?, human))
            }
            MailCmd::Send => {
                mail::list(&ctx, &actor, None, 1).await?; // 로컬 운영자만
                let mailer =
                    maru_storage::mailer::FileMailer::new(dir.mail_dir(), &config.site_url);
                let r = mail::deliver_pending(&ctx, &mailer).await?;
                let human = format!(
                    "보냄 {}통, 다시 시도 {}통, 실패 {}통 → {}",
                    r.sent,
                    r.retry,
                    r.failed,
                    mailer.dir().display()
                );
                Ok((to_value(&r)?, human))
            }
        }
    }
    .await;
    db.close().await;
    res
}

async fn tag_cmd(g: &Global, cmd: TagCmd) -> Out {
    let (ctx, _config, db) = open(g).await?;
    let actor = actor(&ctx).await?;
    let res = match cmd {
        TagCmd::List => {
            let list = tags::list(&ctx, &actor).await?;
            let mut human = format!("태그 {}개", list.len());
            for t in &list {
                human.push_str(&format!("\n#{}  글 {}개", t.name, t.posts));
            }
            Ok((to_value(&list)?, human))
        }
    };
    db.close().await;
    res
}

fn file_out(f: &maru_core::model::Attachment, verb: &str) -> Out {
    let human = format!(
        "{verb}: {} ({}KB)\n{}\n본문에 넣기: [{}]({})",
        f.filename,
        f.size / 1024,
        f.url,
        f.filename,
        f.url
    );
    Ok((to_value(f)?, human))
}

/// 사람용 출력에는 본문에 바로 붙일 Markdown 한 줄을 같이 낸다.
fn media_out(m: &maru_core::model::Media, verb: &str) -> Out {
    let alt = m.alt_text.as_deref().unwrap_or("");
    let human = format!(
        "{verb}: {} ({}x{}, {}KB)\n{}\n본문에 넣기: ![{alt}]({})",
        m.id,
        m.width,
        m.height,
        m.size / 1024,
        m.url,
        m.url
    );
    Ok((to_value(m)?, human))
}
