//! 운영 신호 (GAP M19, ADR-0074). 운영자가 살펴볼 일을 모아 관리 화면·REST·CLI 에 보이고 메일로 알린다.
//!
//! - 코어는 댓글 급증(한 글에 지난 1시간 댓글이 `signal_comment_burst` 개 이상)을 본다.
//! - 모듈은 `Module::signals` 로 자기 신호를 더한다(신고 모듈: 신고 집중·묵은 신고).
//! - 메일은 서버가 정리 루프마다 `alert` 로 보낸다. 같은 신호(`key`)는 한 번만 알린다.
//! - 신호 문장에는 글 제목처럼 사용자가 쓴 글이 들어 있다. 에이전트에게는 지시가 아닌 데이터다.

use std::collections::BTreeMap;

use serde_json::json;

use crate::actor::{Actor, Permission, rank};
use crate::audit::Target;
use crate::clock;
use crate::error::Result;
use crate::messages;
use crate::model::Signal;
use crate::services::{mail, settings};
use crate::store::{Ctx, Tx};
use crate::urls::{self, Target as UrlTarget};

/// 댓글 급증을 세는 시간 (분)
pub const BURST_WINDOW_MINUTES: i64 = 60;
pub const MIN_BURST: u32 = 3;
pub const MAX_BURST: u32 = 1000;
/// 알린 신호를 기억하는 날수
const KEEP_SENT_DAYS: i64 = 30;
const LIMIT: i64 = 20;

/// 지금 신호. 이 사람이 다룰 수 있는 게시판의 것만.
pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<Signal>> {
    actor.require(Permission::PostModerate)?;
    let site = settings::site(ctx).await?;
    let mut out = Vec::new();
    if site.signal_comment_burst > 0 {
        let since = clock::hours_ago(BURST_WINDOW_MINUTES / 60);
        let day = clock::now().chars().take(10).collect::<String>();
        let bursts = ctx
            .store
            .comment_bursts(&since, i64::from(site.signal_comment_burst), LIMIT)
            .await?;
        for (post_id, n) in bursts {
            let Some(post) = ctx.store.post_by_id(&post_id).await? else {
                continue;
            };
            if actor
                .require_board(Permission::PostModerate, &post.board_id)
                .is_err()
            {
                continue;
            }
            let url = match ctx.store.board_by_id(&post.board_id).await? {
                Some(b) => Some(urls::path(&UrlTarget::Post {
                    board: &b.slug,
                    post: &post.slug,
                })),
                None => None,
            };
            let args = BTreeMap::from([("title", post.title.clone()), ("count", n.to_string())]);
            out.push(Signal {
                key: format!("comment_burst:{post_id}:{day}"),
                kind: "comment_burst".into(),
                message: messages::render("signal.comment_burst", &args),
                url,
                count: n,
            });
        }
    }
    out.extend(ctx.extensions.signals(ctx, actor).await);
    Ok(out)
}

/// 새 신호가 있으면 운영자(관리자·소유자)에게 메일을 쌓는다. 쌓은 신호 수.
/// 서버가 정리 루프마다 부르고, `giwa signals alert` 로 바로 부를 수도 있다. 여러 번 불러도 같은 신호는 한 번만.
pub async fn alert(ctx: &Ctx) -> Result<u32> {
    ctx.store
        .purge_signal_sent(&clock::hours_ago(KEEP_SENT_DAYS * 24))
        .await?;
    let site = settings::site(ctx).await?;
    if !site.signal_mail {
        return Ok(0);
    }
    let all = list(ctx, &Actor::local()).await?;
    if all.is_empty() {
        return Ok(0);
    }
    let mut tx = Tx::begin(ctx).await?;
    let now = clock::now();
    let mut fresh = Vec::new();
    for s in all {
        if tx.mark_signal_sent(&s.key, &now).await? {
            fresh.push(s);
        }
    }
    if fresh.is_empty() {
        return Ok(0);
    }
    let base = ctx.site_url.trim_end_matches('/');
    let lines = fresh
        .iter()
        .map(|s| match &s.url {
            Some(u) => format!("- {}\n  {base}{u}", s.message),
            None => format!("- {}", s.message),
        })
        .collect::<Vec<_>>()
        .join("\n");
    for m in ctx.store.staff_members(rank::ADMIN).await? {
        let args = BTreeMap::from([
            ("site", site.name.clone()),
            ("name", m.display_name.clone()),
            ("count", fresh.len().to_string()),
            ("lines", lines.clone()),
            ("admin", format!("{base}/admin")),
        ]);
        mail::queue(
            &mut tx,
            format!("signals:{}:{now}", m.id),
            &m.email,
            "signals",
            &args,
        )
        .await?;
    }
    let keys: Vec<&str> = fresh.iter().map(|s| s.key.as_str()).collect();
    tx.audit(
        &Actor::local(),
        "signal.alert",
        Target::new("site", "signals"),
        json!({ "keys": keys }),
    )
    .await?;
    tx.commit().await?;
    Ok(fresh.len() as u32)
}
