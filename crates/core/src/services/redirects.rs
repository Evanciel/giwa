//! 옮긴 주소 (ADR-0128). 살아 있는 화면이 먼저다: 공개 화면이 404 일 때만 찾는다.
//!
//! 슬러그를 바꾸거나 글을 옮기면 저절로 남는다. 다른 사이트에서 옮겨 올 때는 직접 더한다.

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::actor::{Actor, Permission};
use crate::audit::Target;
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::Redirect;
use crate::store::{Ctx, Tx};

const MAX_PATH: usize = 500;
/// 서비스가 쓰는 주소는 옮길 수 없다.
const RESERVED: [&str; 16] = [
    "/admin",
    "/api/",
    "/assets/",
    "/media/",
    "/files/",
    "/fonts/",
    "/__preview",
    "/m/",
    "/mcp",
    "/login",
    "/logout",
    "/signup",
    "/me",
    "/sitemap.xml",
    "/rss.xml",
    "/robots.txt",
];
/// 고리를 찾을 때 따라가는 횟수.
const MAX_HOPS: usize = 16;
/// `Location` 머리에 그대로 둘 수 없는 글자. 한글은 언제나 인코딩된다.
const LOCATION: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRedirect {
    /// 옮기기 전 주소. `/` 로 시작하는 이 사이트의 경로
    pub from: String,
    /// 옮긴 주소. 이 사이트의 경로이거나 http(s) 주소
    pub to: String,
}

/// 저장과 비교는 디코딩한 글자로 한다. 같은 주소가 인코딩에 따라 갈리지 않는다.
fn canonical(path: &str) -> String {
    percent_decode_str(path.trim())
        .decode_utf8_lossy()
        .into_owned()
}

fn bad(key: &'static str, path: &str) -> Error {
    Error::new(Code::Validation)
        .with_message(key)
        .arg("path", path)
        .next("maru redirect list")
}

fn check_from(from: &str) -> Result<()> {
    let plain = from.starts_with('/')
        && !from.starts_with("//")
        && from.len() > 1
        && from.len() <= MAX_PATH
        && !from.contains(['?', '#', '\\'])
        && !from.chars().any(char::is_control);
    if !plain || RESERVED.iter().any(|r| from.starts_with(r)) {
        return Err(bad("error.redirect_from", from));
    }
    Ok(())
}

fn check_to(to: &str) -> Result<()> {
    let local = to.starts_with('/') && !to.starts_with("//") && !to.contains('\\');
    let remote = to.starts_with("https://") || to.starts_with("http://");
    if !(local || remote) || to.len() > MAX_PATH || to.chars().any(char::is_control) {
        return Err(bad("error.redirect_to", to));
    }
    Ok(())
}

pub async fn list(ctx: &Ctx, actor: &Actor) -> Result<Vec<Redirect>> {
    actor.require(Permission::SiteView)?;
    ctx.store.list_redirects().await
}

/// 있으면 돌려준다. changeset 의 역연산이 지금 값을 알아야 한다.
pub async fn get(ctx: &Ctx, actor: &Actor, from: &str) -> Result<Option<Redirect>> {
    actor.require(Permission::SiteView)?;
    ctx.store.redirect_of(&canonical(from)).await
}

/// 같은 `from` 이 있으면 새 값으로 바꾼다.
pub async fn add(ctx: &Ctx, actor: &Actor, input: NewRedirect) -> Result<Redirect> {
    actor.require(Permission::StructureEdit)?;
    let from = canonical(&input.from);
    let to = canonical(&input.to);
    check_from(&from)?;
    check_to(&to)?;
    if from == to {
        return Err(bad("error.redirect_loop", &from));
    }
    let mut hop = to.clone();
    for _ in 0..MAX_HOPS {
        match ctx.store.redirect_of(&hop).await? {
            Some(next) if next.to_path == from => return Err(bad("error.redirect_loop", &from)),
            Some(next) => hop = next.to_path,
            None => break,
        }
    }
    let row = Redirect {
        from_path: from,
        to_path: to,
        created_at: clock::now(),
    };
    let mut tx = Tx::begin(ctx).await?;
    tx.put_redirect(&row).await?;
    tx.audit(
        actor,
        "redirect.add",
        Target::new("redirect", row.from_path.clone()),
        json!({ "from": row.from_path, "to": row.to_path }),
    )
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn remove(ctx: &Ctx, actor: &Actor, from: &str) -> Result<Redirect> {
    actor.require(Permission::StructureEdit)?;
    let from = canonical(from);
    let row = ctx.store.redirect_of(&from).await?.ok_or_else(|| {
        Error::new(Code::NotFound)
            .with_message("error.redirect_not_found")
            .arg("path", from.clone())
            .next("maru redirect list")
    })?;
    let mut tx = Tx::begin(ctx).await?;
    tx.delete_redirect(&from).await?;
    tx.audit(
        actor,
        "redirect.remove",
        Target::new("redirect", from.clone()),
        json!({ "from": row.from_path, "to": row.to_path }),
    )
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// 없는 주소를 받았을 때 보낼 곳. `Location` 에 그대로 쓸 수 있게 인코딩해 돌려준다.
pub async fn resolve(ctx: &Ctx, path: &str) -> Result<Option<String>> {
    let found = ctx.store.redirect_of(&canonical(path)).await?;
    Ok(found.map(|r| utf8_percent_encode(&r.to_path, LOCATION).to_string()))
}

/// 슬러그를 바꾸거나 글을 옮겼다. 바꾸는 쪽의 기록에 함께 남으니 따로 감사하지 않는다.
pub(crate) async fn moved(tx: &mut Tx, from: &str, to: &str) -> Result<()> {
    tx.put_redirect(&Redirect {
        from_path: canonical(from),
        to_path: canonical(to),
        created_at: clock::now(),
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 옮길_주소는_사이트_안의_평범한_경로다() {
        assert!(check_from("/old").is_ok());
        assert!(check_from("/p/회사소개").is_ok());
        for bad in [
            "old",
            "/",
            "//evil.example",
            "/x?y=1",
            "/x#y",
            "/admin",
            "/admin/pages",
            "/api/v1/posts",
            "/login",
        ] {
            assert!(check_from(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn 보낼_곳은_경로이거나_http_주소다() {
        assert!(check_to("/p/new").is_ok());
        assert!(check_to("https://example.com/x").is_ok());
        for bad in ["javascript:alert(1)", "//evil.example", "ftp://x", "new"] {
            assert!(check_to(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn 인코딩이_달라도_같은_주소다() {
        assert_eq!(canonical("/p/%ED%9A%8C%EC%82%AC"), "/p/회사");
        assert_eq!(canonical("/p/회사"), "/p/회사");
    }
}
