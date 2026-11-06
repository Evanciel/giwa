//! `maru expose` (GAP S8, ADR-0118). 외부 공개는 터널로만 한다 (원칙 6). 포트포워딩은 다루지 않는다.
//!
//! - `--quick`: 계정 없이 쓰는 임시 주소. 끌 때까지 앞에서 돈다(`serve` 처럼 첫 줄에 결과를 낸다).
//! - `--hostname`: 내 도메인. 설정을 맞추고 `cloudflared` 명령 순서를 돌려준다. 로그인에 브라우저가 필요하다.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use maru_core::config::{Config, is_loopback_host};
use maru_core::services::settings;
use maru_core::{Code, Error, Result, clock};
use maru_storage::DataDir;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::commands::{Out, actor, open, shutdown_signal};
use crate::output;
use crate::{ExposeArgs, ExposeCmd, Global};

/// 공식 설치 안내. 자동으로 받지 않는다.
const DOWNLOADS: &str = "https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/downloads/";
const STATE_FILE: &str = "expose.json";
const QUICK_WAIT: Duration = Duration::from_secs(60);

/// 돌고 있는 임시 터널 기록. 다시 실행해도 터널을 또 만들지 않고, 프로세스가 사라져도 끌 수 있게 둔다.
#[derive(Debug, Serialize, Deserialize)]
struct TunnelState {
    url: String,
    pid: u32,
    /// `cloudflared --metrics` 주소. 여기가 답하면 터널이 살아 있다.
    metrics: String,
    target: String,
    started_at: String,
}

pub(crate) async fn run(g: &Global, args: ExposeArgs) -> ExitCode {
    let res = match args.cmd {
        Some(ExposeCmd::Status) => status(g).await,
        Some(ExposeCmd::Stop) => stop(g).await,
        None if !args.cloudflare => Err(Error::new(Code::Usage)
            .with_message("error.usage_detail")
            .arg("detail", "공개 방식을 고르세요")
            .next("maru expose --cloudflare")),
        None if args.quick => return quick(g, args.to).await,
        None => match args.hostname {
            Some(host) => named(g, &host, &args.tunnel, args.to).await,
            None => guide(g, args.to),
        },
    };
    match res {
        Ok((data, human)) => output::success(data, g.json, |_| human),
        Err(e) => output::error(&e, g.json),
    }
}

/// PATH 에서 찾는다. 윈도우의 npm 설치본은 `.cmd` 라 이름을 다 본다.
pub(crate) fn find_cloudflared() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["cloudflared.exe", "cloudflared.cmd", "cloudflared.bat"]
    } else {
        &["cloudflared"]
    };
    let path = std::env::var_os("PATH")?;
    path_dirs(&path.to_string_lossy())
        .into_iter()
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

/// `std::env::split_paths` 는 짝이 안 맞는 따옴표 하나에 뒤의 경로를 모두 붙여 버린다.
/// 그런 PATH 는 실제로 있고 셸은 잘 찾으니, 구분자로 나누고 따옴표만 뗀다.
fn path_dirs(path: &str) -> Vec<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    path.split(sep)
        .map(|d| d.trim().trim_matches('"'))
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn require_cloudflared(g: &Global) -> Result<PathBuf> {
    find_cloudflared().ok_or_else(|| {
        Error::new(Code::External)
            .with_message("error.cloudflared_missing")
            .arg("url", DOWNLOADS)
            .next(format!(
                "maru --data \"{}\" expose status",
                g.data.display()
            ))
    })
}

fn version(path: &Path) -> Option<String> {
    let out = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|l| l.trim().to_owned())
}

fn state_path(g: &Global) -> PathBuf {
    DataDir::new(&g.data).root().join(STATE_FILE)
}

fn read_state(g: &Global) -> Option<TunnelState> {
    let raw = std::fs::read_to_string(state_path(g)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn remove_state(g: &Global) {
    let _ = std::fs::remove_file(state_path(g));
}

/// 터널을 보낼 주소. `0.0.0.0` 으로 듣는 서버에는 루프백으로 붙는다.
fn target_addr(bind: &str) -> Result<SocketAddr> {
    let mut addr: SocketAddr = bind.parse().map_err(|_| {
        Error::new(Code::Validation)
            .with_message("error.config_bind")
            .arg("value", bind.to_owned())
    })?;
    if addr.ip().is_unspecified() {
        addr.set_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    Ok(addr)
}

fn no_server(g: &Global, target: SocketAddr) -> Error {
    Error::new(Code::Validation)
        .with_message("error.expose_no_server")
        .arg("target", target.to_string())
        .next(format!(
            "maru serve --data \"{}\" --bind {target}",
            g.data.display()
        ))
}

/// 터널 로그에서 임시 주소를 꺼낸다. `api.trycloudflare.com` 은 발급 창구라 주소가 아니다.
fn quick_url(line: &str) -> Option<String> {
    let from = line.find("https://")?;
    let url: String = line[from..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '.' | '-'))
        .collect();
    let host = url.strip_prefix("https://")?;
    let name = host.strip_suffix(".trycloudflare.com")?;
    (!name.is_empty() && name != "api" && !name.contains(['/', '.'])).then_some(url)
}

/// 터널 프로세스를 끝낸다. 윈도우 npm 설치본은 껍데기 아래에 진짜 프로세스가 있어 나무째 끈다.
fn stop_process(pid: u32) {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        c
    } else {
        let mut c = Command::new("kill");
        c.arg(pid.to_string());
        c
    };
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 문서에 있는 `/metrics` 가 답하면 터널 프로세스가 살아 있다.
async fn tunnel_alive(metrics: &str) -> bool {
    let wait = Duration::from_secs(2);
    let exchange = async {
        let mut s = tokio::net::TcpStream::connect(metrics).await.ok()?;
        let req = format!("GET /metrics HTTP/1.1\r\nHost: {metrics}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut head = [0u8; 12];
        s.read_exact(&mut head).await.ok()?;
        Some(head.ends_with(b"200"))
    };
    tokio::time::timeout(wait, exchange)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

fn free_metrics_addr() -> Result<String> {
    let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(Error::internal)?;
    Ok(l.local_addr().map_err(Error::internal)?.to_string())
}

fn notes(site_url: &str) -> Vec<String> {
    vec![
        "사이트 전체(관리 화면·REST·/mcp 포함)가 인터넷에 열립니다.".to_owned(),
        "임시 주소입니다. 끄면 사라지고 다시 켜면 바뀝니다. 운영에는 --hostname 을 쓰세요."
            .to_owned(),
        format!("site_url 은 그대로 {site_url} 입니다. RSS·편지·공유 링크는 이 주소를 가리킵니다."),
    ]
}

/// 임시 터널을 켜고 끌 때까지 앞에서 돈다. 이미 돌고 있으면 그 주소를 돌려주고 끝낸다.
async fn quick(g: &Global, to: Option<String>) -> ExitCode {
    match quick_inner(g, to).await {
        Ok(code) => code,
        Err(e) => output::error(&e, g.json),
    }
}

async fn quick_inner(g: &Global, to: Option<String>) -> Result<ExitCode> {
    let path = require_cloudflared(g)?;
    let (ctx, mut config, db) = open(g).await?;
    let res = async {
        let who = actor(&ctx).await?;
        let target = target_addr(to.as_deref().unwrap_or(&config.bind))?;
        settings::record_expose(&ctx, &who, json!({ "mode": "quick", "target": target })).await?;
        Ok::<_, Error>(target)
    }
    .await;
    db.close().await;
    let target = res?;

    if let Some(s) = read_state(g) {
        if tunnel_alive(&s.metrics).await {
            let human = format!("이미 공개 중입니다: {}\n끄려면 maru expose stop", s.url);
            let data = json!({
                "mode": "quick",
                "url": s.url,
                "target": s.target,
                "pid": s.pid,
                "already_running": true,
                "notes": notes(&config.site_url),
            });
            return Ok(output::success(data, g.json, |_| human));
        }
        remove_state(g);
    }

    if crate::doctor::probe_maru(target).await != Some(true) {
        return Err(no_server(g, target));
    }
    // 터널 뒤에서는 모든 방문자가 루프백으로 보인다. 건네주는 주소를 믿게 켠다 (ADR-0118).
    let restart_required = !config.trust_tunnel;
    if restart_required {
        config.trust_tunnel = true;
        DataDir::new(&g.data).write_config(&config)?;
    }

    let metrics = free_metrics_addr()?;
    let mut child = Command::new(&path)
        .args(["tunnel", "--no-autoupdate", "--metrics", &metrics, "--url"])
        .arg(format!("http://{target}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(Error::internal)?;
    let stderr = child.stderr.take().ok_or_else(|| {
        Error::internal(std::io::Error::other(
            "cloudflared 의 stderr 를 받지 못했습니다",
        ))
    })?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut tail: VecDeque<String> = VecDeque::new();
    let mut url = None;
    let mut connected = false;
    let waited = tokio::time::timeout(QUICK_WAIT, async {
        while let Some(line) = rx.recv().await {
            url = url.take().or_else(|| quick_url(&line));
            connected = connected || line.contains("Registered tunnel connection");
            if tail.len() == 6 {
                tail.pop_front();
            }
            tail.push_back(line);
            if url.is_some() && connected {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    let url = match (waited, url) {
        (true, Some(url)) => url,
        _ => {
            stop_process(child.id());
            let _ = child.wait();
            return Err(Error::new(Code::External)
                .with_message("error.expose_failed")
                .arg("detail", tail.into_iter().collect::<Vec<_>>().join(" / "))
                .next(format!(
                    "maru --data \"{}\" expose status",
                    g.data.display()
                )));
        }
    };

    let state = TunnelState {
        url: url.clone(),
        pid: child.id(),
        metrics,
        target: target.to_string(),
        started_at: clock::now(),
    };
    let body = serde_json::to_string(&state).map_err(Error::internal)?;
    std::fs::write(state_path(g), body).map_err(Error::internal)?;

    let notes = notes(&config.site_url);
    let mut human = format!("공개했습니다: {url}\n멈추려면 Ctrl+C 또는 maru expose stop");
    for n in &notes {
        human.push_str(&format!("\n- {n}"));
    }
    if restart_required {
        human.push_str("\n- 방문자 주소를 구분하려면 maru serve 를 다시 켜세요.");
    }
    let data = json!({
        "mode": "quick",
        "url": url,
        "target": target.to_string(),
        "pid": child.id(),
        "already_running": false,
        "restart_required": restart_required,
        "notes": notes,
    });
    output::success(data, g.json, |_| human);

    let ended = tokio::select! {
        () = shutdown_signal() => false,
        () = async { while rx.recv().await.is_some() {} } => true,
    };
    stop_process(child.id());
    let _ = child.wait();
    remove_state(g);
    if ended {
        let e = Error::new(Code::External)
            .with_message("error.expose_failed")
            .arg("detail", "터널 프로세스가 끝났습니다");
        return Ok(output::error(&e, g.json));
    }
    Ok(ExitCode::SUCCESS)
}

/// 도메인 이름만 남긴다. 명령에 그대로 들어가니 글자를 좁힌다.
fn clean_hostname(raw: &str) -> Result<String> {
    let host = raw
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_lowercase();
    let good = host.contains('.')
        && !is_loopback_host(&host)
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if good {
        Ok(host)
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.expose_hostname")
            .arg("value", raw.to_owned())
            .next("maru expose --cloudflare --hostname blog.example.com"))
    }
}

/// 내 도메인: 설정을 맞추고 명령 순서를 돌려준다. 계정 쪽 일은 브라우저 로그인이 있어 대신 하지 않는다.
async fn named(g: &Global, raw: &str, tunnel: &str, to: Option<String>) -> Out {
    let path = require_cloudflared(g)?;
    let host = clean_hostname(raw)?;
    if tunnel.is_empty()
        || !tunnel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(Error::new(Code::Validation)
            .with_message("error.usage_detail")
            .arg(
                "detail",
                format!("터널 이름은 영문·숫자·-·_ 만 됩니다: {tunnel}"),
            )
            .next("maru expose --cloudflare --hostname blog.example.com --tunnel maru"));
    }
    let (ctx, mut config, db) = open(g).await?;
    let res = async {
        let who = actor(&ctx).await?;
        let target = target_addr(to.as_deref().unwrap_or(&config.bind))?;
        let site_url = format!("https://{host}");
        let changed = config.site_url != site_url || !config.trust_tunnel;
        if changed {
            settings::record_expose(
                &ctx,
                &who,
                json!({ "mode": "hostname", "hostname": host, "site_url": site_url }),
            )
            .await?;
            config.site_url = site_url.clone();
            config.trust_tunnel = true;
            DataDir::new(&g.data).write_config(&config)?;
        }
        let running = crate::doctor::probe_maru(target).await == Some(true);
        let steps = vec![
            json!({ "command": "cloudflared tunnel login", "note": "브라우저에서 Cloudflare 에 로그인하고 도메인을 고릅니다. 사람이 해야 합니다." }),
            json!({ "command": format!("cloudflared tunnel create {tunnel}") }),
            json!({ "command": format!("cloudflared tunnel route dns {tunnel} {host}") }),
            json!({ "command": format!("cloudflared tunnel run --url http://{target} {tunnel}"), "note": "켜 두는 동안 공개됩니다." }),
        ];
        let mut human = format!("site_url 을 {site_url} 로 맞췄습니다. 아래 순서로 실행하세요:");
        for s in &steps {
            human.push_str(&format!("\n  {}", s["command"].as_str().unwrap_or_default()));
        }
        if changed && running {
            human.push_str("\nmaru serve 를 다시 켜야 적용됩니다.");
        }
        Ok::<_, Error>((
            json!({
                "mode": "hostname",
                "hostname": host,
                "site_url": site_url,
                "changed": changed,
                "restart_required": changed && running,
                "server_running": running,
                "cloudflared": path.display().to_string(),
                "steps": steps,
            }),
            human,
        ))
    }
    .await;
    db.close().await;
    res
}

/// 방식을 고르지 않았을 때: 아무것도 바꾸지 않고 두 길을 보여 준다.
fn guide(g: &Global, to: Option<String>) -> Out {
    let path = require_cloudflared(g)?;
    let config = DataDir::new(&g.data).read_config()?;
    let target = target_addr(to.as_deref().unwrap_or(&config.bind))?;
    let data = g.data.display();
    let quick = format!("maru --data \"{data}\" expose --cloudflare --quick");
    let named = format!("maru --data \"{data}\" expose --cloudflare --hostname <내 도메인>");
    let human = format!(
        "cloudflared: {}\n시험 삼아 임시 주소로: {quick}\n내 도메인으로: {named}",
        path.display()
    );
    Ok((
        json!({
            "cloudflared": { "path": path.display().to_string(), "version": version(&path) },
            "target": target.to_string(),
            "options": [
                { "mode": "quick", "command": quick, "note": "계정이 필요 없습니다. 주소가 매번 바뀌고 시험용입니다." },
                { "mode": "hostname", "command": named, "note": "Cloudflare 계정과 도메인이 필요합니다. 운영은 이쪽입니다." },
            ],
        }),
        human,
    ))
}

async fn status(g: &Global) -> Out {
    let config: Config = DataDir::new(&g.data).read_config()?;
    let cloudflared = find_cloudflared();
    let server = match target_addr(&config.bind) {
        Ok(addr) => crate::doctor::probe_maru(addr).await == Some(true),
        Err(_) => false,
    };
    let tunnel = match read_state(g) {
        Some(s) => {
            let alive = tunnel_alive(&s.metrics).await;
            Some(json!({
                "url": s.url,
                "pid": s.pid,
                "target": s.target,
                "started_at": s.started_at,
                "alive": alive,
            }))
        }
        None => None,
    };
    let mut human = match &cloudflared {
        Some(p) => format!("cloudflared: {}", p.display()),
        None => format!("cloudflared: 없음 ({DOWNLOADS})"),
    };
    human.push_str(&format!(
        "\nsite_url: {}\n서버({}): {}\n방문자 주소 믿기: {}",
        config.site_url,
        config.bind,
        if server { "켜짐" } else { "꺼짐" },
        if config.trust_tunnel {
            "켜짐"
        } else {
            "꺼짐"
        },
    ));
    match &tunnel {
        Some(t) if t["alive"] == true => {
            human.push_str(&format!(
                "\n임시 터널: {}",
                t["url"].as_str().unwrap_or_default()
            ));
        }
        Some(_) => human.push_str("\n임시 터널: 기록은 있지만 응답이 없습니다"),
        None => human.push_str("\n임시 터널: 없음"),
    }
    let data: Value = json!({
        "cloudflared": cloudflared.as_ref().map(|p| json!({
            "path": p.display().to_string(),
            "version": version(p),
        })),
        "site_url": config.site_url,
        "bind": config.bind,
        "server_running": server,
        "trust_tunnel": config.trust_tunnel,
        "tunnel": tunnel,
    });
    Ok((data, human))
}

/// 임시 터널을 끈다. 없어도 성공이다.
async fn stop(g: &Global) -> Out {
    let Some(s) = read_state(g) else {
        return Ok((
            json!({ "stopped": false }),
            "돌고 있는 임시 터널이 없습니다.".to_owned(),
        ));
    };
    // 답이 없으면 그 pid 는 이미 다른 프로그램의 것일 수 있어 건드리지 않는다.
    let alive = tunnel_alive(&s.metrics).await;
    if alive {
        stop_process(s.pid);
    }
    remove_state(g);
    let human = if alive {
        format!("공개를 멈췄습니다: {}", s.url)
    } else {
        "이미 멈춘 터널이라 기록만 지웠습니다.".to_owned()
    };
    Ok((json!({ "stopped": alive, "url": s.url }), human))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 짝이_안_맞는_따옴표가_있어도_뒤의_경로를_찾는다() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let path = format!("/a{sep}/git/cmd\"{sep}{sep}\"/q r\"{sep}/npm");
        assert_eq!(
            path_dirs(&path),
            ["/a", "/git/cmd", "/q r", "/npm"].map(PathBuf::from)
        );
    }

    #[test]
    fn 임시_주소를_로그에서_꺼낸다() {
        let line = "2026-11-06T07:00:00Z INF |  https://warm-owl-tiny.trycloudflare.com   |";
        assert_eq!(
            quick_url(line).as_deref(),
            Some("https://warm-owl-tiny.trycloudflare.com")
        );
        for not in [
            r#"ERR failed to request quick Tunnel: Post "https://api.trycloudflare.com/tunnel""#,
            "INF Requesting new quick Tunnel on trycloudflare.com...",
            "INF https://developers.cloudflare.com/cloudflare-one/",
            "INF https://trycloudflare.com",
            "INF https://evil.example.com/a.trycloudflare.com",
            "INF https://a.b.trycloudflare.com",
        ] {
            assert_eq!(quick_url(not), None, "{not}");
        }
    }

    #[test]
    fn 도메인만_받는다() {
        assert_eq!(
            clean_hostname(" https://Blog.Example.com/ ").unwrap(),
            "blog.example.com"
        );
        for bad in [
            "localhost",
            "127.0.0.1",
            "없는주소",
            "a b.example.com",
            "x.example.com; rm",
            "-a.example.com",
            "a..com",
        ] {
            assert!(clean_hostname(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn 듣는_주소를_터널_대상으로_바꾼다() {
        assert_eq!(
            target_addr("0.0.0.0:8080").unwrap().to_string(),
            "127.0.0.1:8080"
        );
        assert!(target_addr("주소").is_err());
    }
}
