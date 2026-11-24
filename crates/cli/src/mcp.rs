//! `giwa mcp serve`, `giwa mcp install` (GAP S8, ADR-0117).
//!
//! 토큰 원문은 데이터 디렉터리 `secrets/` 에만 둔다. 클라이언트 설정에는 파일 경로만 적는다.

use std::path::{Path, PathBuf};

use giwa_core::model::Token;
use giwa_core::services::tokens::{self, NewToken};
use giwa_core::{Code, Ctx, Error, Result, Scope};
use giwa_storage::DataDir;
use serde_json::{Map, Value, json};

use crate::commands::{Out, actor, open};
use crate::{Global, McpInstallArgs, McpServeArgs};

const CLIENTS: [&str; 3] = ["claude-code", "claude-desktop", "cursor"];
const SERVER_NAME: &str = "giwa";

/// 쓸 토큰: `--token-file` → `GIWA_TOKEN`. 없으면 운영자 권한으로 돌지 않고 멈춘다.
fn secret(g: &Global, args: &McpServeArgs) -> Result<String> {
    if let Some(path) = &args.token_file {
        let raw = std::fs::read_to_string(path).map_err(|_| {
            Error::new(Code::Validation)
                .with_message("error.usage_detail")
                .arg(
                    "detail",
                    format!("토큰 파일을 읽지 못했습니다: {}", path.display()),
                )
                .next(install_command(g))
        })?;
        return Ok(raw.trim().to_owned());
    }
    match giwa_core::env::var("TOKEN") {
        Some(t) if !t.trim().is_empty() => Ok(t.trim().to_owned()),
        _ => Err(Error::new(Code::Unauthorized)
            .with_message("error.mcp_token")
            .next(install_command(g))),
    }
}

fn install_command(g: &Global) -> String {
    format!(
        "giwa --data \"{}\" mcp install --client claude-code",
        g.data.display()
    )
}

/// 표준 입출력 서버. stdout 은 프로토콜 전용이라 성공해도 아무것도 찍지 않는다.
pub(crate) async fn serve(g: &Global, args: McpServeArgs) -> Result<()> {
    let secret = secret(g, &args)?;
    let (mut ctx, config, db) = open(g).await?;
    // 미리보기 주소가 지금 도는 서버를 가리키게 한다.
    ctx.site_url = crate::commands::running_url(g, &config, &config.site_url).await;
    let res = async {
        tokens::authenticate(&ctx, &secret)
            .await
            .map_err(|e| e.next(install_command(g)))?;
        let registry = crate::modules::registry(&db, &g.data)?;
        let sandbox = giwa_storage::sandbox::FsSandbox::new(&DataDir::new(&g.data), db.clone());
        giwa_agent::serve_stdio(
            ctx.clone(),
            registry,
            secret,
            Some(std::sync::Arc::new(sandbox)),
        )
        .await
    }
    .await;
    db.close().await;
    res
}

fn home() -> Result<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| config_required("집 디렉터리를 찾지 못했습니다."))
}

fn config_required(detail: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.usage_detail")
        .arg("detail", detail.to_owned())
        .next("giwa mcp install --client <클라이언트> --config <설정 파일>")
}

/// 클라이언트 설정 파일의 기본 자리.
fn default_config(client: &str) -> Result<PathBuf> {
    Ok(match client {
        "claude-code" => std::env::current_dir()
            .map_err(Error::internal)?
            .join(".mcp.json"),
        "cursor" => home()?.join(".cursor").join("mcp.json"),
        _ if cfg!(windows) => std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| config_required("APPDATA 를 찾지 못했습니다."))?
            .join("Claude")
            .join("claude_desktop_config.json"),
        _ if cfg!(target_os = "macos") => home()?
            .join("Library/Application Support/Claude")
            .join("claude_desktop_config.json"),
        _ => home()?
            .join(".config/Claude")
            .join("claude_desktop_config.json"),
    })
}

/// `mcpServers.giwa` 만 넣고 나머지는 그대로 둔다. 읽을 수 없는 파일은 덮어쓰지 않는다.
fn merge_config(path: &Path, entry: Value) -> Result<bool> {
    let mut root: Map<String, Value> = match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Map::new(),
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            config_required(&format!(
                "설정 파일이 JSON 객체가 아니라 건드리지 않았습니다: {} ({e})",
                path.display()
            ))
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(Error::internal(e)),
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| config_required("설정 파일의 mcpServers 가 객체가 아닙니다."))?;
    if servers.get(SERVER_NAME) == Some(&entry) {
        return Ok(false);
    }
    servers.insert(SERVER_NAME.to_owned(), entry);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(Error::internal)?;
    }
    let body = serde_json::to_string_pretty(&Value::Object(root)).map_err(Error::internal)?;
    let tmp = path.with_extension("giwa-tmp");
    std::fs::write(&tmp, body + "\n").map_err(Error::internal)?;
    std::fs::rename(&tmp, path).map_err(Error::internal)?;
    Ok(true)
}

fn write_secret(path: &Path, secret: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(Error::internal)?;
    }
    std::fs::write(path, secret).map_err(Error::internal)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(Error::internal)?;
    }
    Ok(())
}

fn same_scopes(token: &Token, wanted: &[Scope]) -> bool {
    let mut have = token.scopes.clone();
    let mut want = wanted.to_vec();
    have.sort();
    want.sort();
    have == want
}

/// 클라이언트 몫의 토큰을 맞춘다. 파일의 토큰이 살아 있고 스코프가 같으면 그대로 둔다(멱등).
async fn ensure_token(
    ctx: &Ctx,
    who: &giwa_core::Actor,
    name: &str,
    file: &Path,
    wanted: &[Scope],
) -> Result<(Token, bool)> {
    let live: Vec<Token> = tokens::list(ctx, who)
        .await?
        .into_iter()
        .filter(|t| t.name == name && t.revoked_at.is_none())
        .collect();
    let saved_works = match std::fs::read_to_string(file) {
        Ok(s) => tokens::authenticate(ctx, s.trim()).await.is_ok(),
        Err(_) => false,
    };
    if saved_works
        && let [token] = live.as_slice()
        && (wanted.is_empty() || same_scopes(token, wanted))
    {
        return Ok((token.clone(), false));
    }
    for old in &live {
        tokens::revoke(ctx, who, &old.id).await?;
    }
    let issued = tokens::create(
        ctx,
        who,
        NewToken {
            name: name.to_owned(),
            scopes: wanted.to_vec(),
            board_ids: None,
            auto_apply: false,
            expires_at: None,
        },
    )
    .await?;
    write_secret(file, &issued.secret)?;
    Ok((issued.token, true))
}

pub(crate) async fn install(g: &Global, args: McpInstallArgs) -> Out {
    if !CLIENTS.contains(&args.client.as_str()) {
        return Err(Error::new(Code::Validation)
            .with_message("error.usage_detail")
            .arg(
                "detail",
                format!(
                    "모르는 클라이언트입니다: {} ({})",
                    args.client,
                    CLIENTS.join(", ")
                ),
            )
            .next(install_command(g)));
    }
    let mut wanted = Vec::new();
    for s in &args.scopes {
        wanted.push(Scope::parse(s).ok_or_else(|| {
            Error::new(Code::Validation)
                .with_message("error.scope_unknown")
                .arg("value", s.clone())
        })?);
    }
    let config_path = match args.config {
        Some(p) => p,
        None => default_config(&args.client)?,
    };
    let (ctx, _config, db) = open(g).await?;
    let res = async {
        let who = actor(&ctx).await?;
        let data = std::path::absolute(&g.data).map_err(Error::internal)?;
        let name = format!("mcp-{}", args.client);
        let token_file = DataDir::new(&data)
            .secrets_dir()
            .join(format!("{name}.token"));
        let (token, created) = ensure_token(&ctx, &who, &name, &token_file, &wanted).await?;
        let exe = std::env::current_exe().map_err(Error::internal)?;
        let entry = json!({
            "command": exe.display().to_string(),
            "args": [
                "--data", data.display().to_string(),
                "mcp", "serve",
                "--token-file", token_file.display().to_string(),
            ],
        });
        let written = if args.print {
            false
        } else {
            merge_config(&config_path, entry.clone())?
        };
        let scopes: Vec<&str> = token.scopes.iter().map(|s| s.as_str()).collect();
        let human = if args.print {
            format!(
                "{} 설정에 아래 항목을 넣으세요 (토큰은 파일에만 있습니다):\n{}",
                args.client,
                serde_json::to_string_pretty(&json!({ "mcpServers": { SERVER_NAME: entry } }))
                    .map_err(Error::internal)?
            )
        } else {
            format!(
                "{} 에 연결했습니다: {}\n토큰 {} ({}). 클라이언트를 다시 켜면 툴이 보입니다.",
                args.client,
                config_path.display(),
                token.name,
                scopes.join(", ")
            )
        };
        Ok::<_, Error>((
            json!({
                "client": args.client,
                "config": config_path.display().to_string(),
                "written": written,
                "server": { SERVER_NAME: entry },
                "token": { "id": token.id, "name": token.name, "scopes": scopes, "created": created },
                "token_file": token_file.display().to_string(),
            }),
            human,
        ))
    }
    .await;
    db.close().await;
    res
}
