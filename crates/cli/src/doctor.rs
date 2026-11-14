//! `giwa doctor`. 설치 상태를 항목별로 점검한다.
//!
//! 결과는 `pass`, `warn`, `fail`, `unknown` 넷 중 하나다. 재지 못한 항목은 통과로 치지 않고
//! `unknown`(못 쟀다)으로 드러낸다. `fail` 이 하나라도 있으면 종료 코드가 0 이 아니다.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use giwa_core::Error;
use giwa_storage::DataDir;
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{Global, output};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pass,
    Warn,
    Fail,
    Unknown,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "통과",
            Self::Warn => "주의",
            Self::Fail => "실패",
            Self::Unknown => "못 쟀다",
        }
    }
}

#[derive(Debug, Serialize)]
struct Check {
    id: &'static str,
    status: Status,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

fn check(id: &'static str, status: Status, message: impl Into<String>) -> Check {
    Check {
        id,
        status,
        message: message.into(),
        next: None,
    }
}

impl Check {
    fn next(mut self, cmd: impl Into<String>) -> Self {
        self.next = Some(cmd.into());
        self
    }
}

pub async fn run(g: &Global) -> ExitCode {
    let checks = collect(g).await;
    let failed = checks.iter().any(|c| c.status == Status::Fail);
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    let summary = json!({
        "pass": count(Status::Pass),
        "warn": count(Status::Warn),
        "fail": count(Status::Fail),
        "unknown": count(Status::Unknown),
    });
    let data = json!({ "healthy": !failed, "summary": summary, "checks": checks });
    let mut human = String::new();
    for c in &checks {
        human.push_str(&format!("[{}] {}: {}\n", c.status.label(), c.id, c.message));
        if let Some(n) = &c.next {
            human.push_str(&format!("       다음: {n}\n"));
        }
    }
    human.push_str(&format!(
        "통과 {}, 주의 {}, 실패 {}, 못 쟀다 {}",
        count(Status::Pass),
        count(Status::Warn),
        count(Status::Fail),
        count(Status::Unknown)
    ));
    output::success(data, g.json, |_| human);
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

async fn collect(g: &Global) -> Vec<Check> {
    let dir = DataDir::new(&g.data);
    let shown = dir.root().display().to_string();
    let mut out = vec![check(
        "version",
        Status::Pass,
        format!("giwa {}", env!("CARGO_PKG_VERSION")),
    )];

    if !dir.is_initialized() {
        out.push(
            check(
                "data_dir",
                Status::Fail,
                format!("초기화된 데이터 디렉터리가 아닙니다: {shown}"),
            )
            .next(format!("giwa init --data \"{shown}\" --yes")),
        );
        for id in ["config", "database", "owner", "port"] {
            out.push(check(
                id,
                Status::Unknown,
                "데이터 디렉터리가 없어 재지 못했습니다.",
            ));
        }
        return out;
    }
    out.push(writable(&dir));

    let config = match dir.read_config() {
        Ok(c) => {
            out.push(check(
                "config",
                Status::Pass,
                format!("bind {}, site_url {}", c.bind, c.site_url),
            ));
            Some(c)
        }
        Err(e) => {
            out.push(check("config", Status::Fail, e.message()));
            None
        }
    };

    match giwa_storage::open(&dir).await {
        Ok((ctx, _config, db)) => {
            out.push(database(&db).await);
            out.push(owner(&ctx).await);
            out.push(tokens(&ctx).await);
            db.close().await;
        }
        Err(e) => {
            out.push(database_error(&e));
            out.push(check(
                "owner",
                Status::Unknown,
                "DB 를 열지 못해 재지 못했습니다.",
            ));
        }
    }

    if let Some(c) = &config {
        out.push(port(&c.bind, &shown).await);
        out.push(site_url(&c.site_url));
    }
    out.push(tunnel());
    out.push(service(g).await);
    out.push(theme(&dir));
    out.push(check(
        "font",
        Status::Warn,
        "Pretendard 글꼴 파일이 들어 있지 않아 시스템 글꼴로 보입니다.",
    ));
    out.push(check(
        "disk_free",
        Status::Unknown,
        "디스크 여유 공간은 재지 않습니다. 운영체제 도구로 확인하세요.",
    ));
    out
}

fn writable(dir: &DataDir) -> Check {
    let probe = dir.root().join(".maru-doctor");
    match std::fs::write(&probe, b"ok").and_then(|()| std::fs::remove_file(&probe)) {
        Ok(()) => check(
            "data_dir",
            Status::Pass,
            format!("쓰기 가능: {}", dir.root().display()),
        ),
        Err(e) => check(
            "data_dir",
            Status::Fail,
            format!("데이터 디렉터리에 쓸 수 없습니다: {e}"),
        ),
    }
}

async fn database(db: &giwa_storage::Db) -> Check {
    let integrity = db.quick_check().await;
    let versions = db.schema_versions().await;
    match (integrity, versions) {
        (Ok(ok), Ok((applied, known))) if ok == "ok" => check(
            "database",
            Status::Pass,
            format!("무결성 정상, 스키마 {applied} (바이너리 {known})"),
        ),
        (Ok(detail), _) if detail != "ok" => check(
            "database",
            Status::Fail,
            format!("무결성 검사 실패: {detail}"),
        ),
        (Err(e), _) | (_, Err(e)) => check("database", Status::Fail, e.message()),
        _ => check("database", Status::Unknown, "결과를 해석하지 못했습니다."),
    }
}

fn database_error(e: &Error) -> Check {
    let mut c = check("database", Status::Fail, e.message());
    if let Some(n) = e.next_commands().first() {
        c = c.next(n.clone());
    }
    c
}

async fn owner(ctx: &giwa_core::Ctx) -> Check {
    match ctx.store.first_member().await {
        Ok(Some(m)) if m.has_password() => check(
            "owner",
            Status::Pass,
            format!("{} (비밀번호 설정됨)", m.email),
        ),
        Ok(Some(m)) => check(
            "owner",
            Status::Warn,
            format!(
                "{} 의 비밀번호가 아직 없습니다. 로그인 링크로만 들어올 수 있습니다.",
                m.email
            ),
        )
        .next("giwa login-link"),
        Ok(None) => check("owner", Status::Fail, "운영자 계정이 없습니다.").next("giwa init --yes"),
        Err(e) => check("owner", Status::Fail, e.message()),
    }
}

async fn tokens(ctx: &giwa_core::Ctx) -> Check {
    match ctx.store.list_tokens().await {
        Ok(list) => {
            let now = giwa_core::clock::now();
            let active = list
                .iter()
                .filter(|t| {
                    t.revoked_at.is_none()
                        && t.expires_at.as_deref().is_none_or(|e| e > now.as_str())
                })
                .count();
            check(
                "tokens",
                Status::Pass,
                format!("API 토큰 {active}개 사용 중 (전체 {}개)", list.len()),
            )
        }
        Err(e) => check("tokens", Status::Fail, e.message()),
    }
}

/// 포트가 비었는지, 차 있다면 giwa 가 쓰는지 본다.
async fn port(bind: &str, data: &str) -> Check {
    let Ok(addr) = bind.parse::<SocketAddr>() else {
        return check(
            "port",
            Status::Fail,
            format!("bind 주소가 올바르지 않습니다: {bind}"),
        );
    };
    if let Ok(l) = tokio::net::TcpListener::bind(addr).await {
        drop(l);
        return check(
            "port",
            Status::Pass,
            format!("{bind} 비어 있음 (서버 꺼짐)"),
        )
        .next(format!("giwa serve --data \"{data}\""));
    }
    match probe_giwa(addr).await {
        Some(true) => check(
            "port",
            Status::Pass,
            format!("{bind} 에서 giwa 서버가 응답합니다"),
        ),
        Some(false) => {
            let mut other = addr;
            other.set_port(addr.port().wrapping_add(1).max(1024));
            check(
                "port",
                Status::Warn,
                format!("{bind} 를 다른 프로그램이 쓰고 있습니다"),
            )
            .next(format!("giwa serve --data \"{data}\" --bind {other}"))
        }
        None => check(
            "port",
            Status::Unknown,
            format!("{bind} 가 사용 중이지만 응답을 확인하지 못했습니다"),
        ),
    }
}

/// `/robots.txt` 에 giwa 가 내는 사이트맵 줄이 있는지로 판단한다.
/// 연결조차 안 되면 `None`(못 쟀다). 연결은 됐는데 giwa 답이 없으면 다른 프로그램이다.
pub(crate) async fn probe_giwa(addr: SocketAddr) -> Option<bool> {
    let wait = Duration::from_secs(3);
    let mut s = tokio::time::timeout(wait, tokio::net::TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    let exchange = async {
        let req = format!("GET /robots.txt HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.ok()?;
        let text = String::from_utf8_lossy(&buf);
        Some(text.starts_with("HTTP/1.1 200") && text.contains("/sitemap.xml"))
    };
    Some(
        tokio::time::timeout(wait, exchange)
            .await
            .ok()
            .flatten()
            .unwrap_or(false),
    )
}

fn site_url(url: &str) -> Check {
    if url.starts_with("https://") {
        return check("site_url", Status::Pass, url.to_owned());
    }
    let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| url.starts_with(p));
    if local {
        check(
            "site_url",
            Status::Pass,
            format!("{url} (로컬 전용. 공개할 때 https 주소로 바꿉니다)"),
        )
    } else {
        check(
            "site_url",
            Status::Warn,
            format!("{url} 는 https 가 아닙니다. 로그인 쿠키에 Secure 가 붙지 않습니다."),
        )
    }
}

/// 없어도 문제는 아니다. 외부 공개할 때만 필요하다.
async fn service(g: &Global) -> Check {
    match crate::service::is_registered(g) {
        Ok(true) => check(
            "service",
            Status::Pass,
            "로그인하면 서버가 뜨도록 등록돼 있습니다.",
        ),
        Ok(false) => check(
            "service",
            Status::Unknown,
            "상시 실행이 등록돼 있지 않습니다. 사이트를 계속 켜 둘 때만 필요합니다.",
        )
        .next("giwa service install"),
        Err(e) => check("service", Status::Unknown, e.message()),
    }
}

fn tunnel() -> Check {
    match crate::expose::find_cloudflared() {
        Some(p) => check(
            "tunnel",
            Status::Pass,
            format!("cloudflared: {}", p.display()),
        ),
        None => check(
            "tunnel",
            Status::Unknown,
            "cloudflared 가 없습니다. 외부 공개할 때만 필요합니다.",
        )
        .next("giwa expose --cloudflare"),
    }
}

fn theme(dir: &DataDir) -> Check {
    let user = dir.themes_dir().join("default");
    if !user.is_dir() {
        return check("theme", Status::Pass, "내장 테마 사용");
    }
    if giwa_themes::compatible(&user) {
        check(
            "theme",
            Status::Pass,
            format!("사용자 테마 사용: {}", user.display()),
        )
    } else {
        check(
            "theme",
            Status::Warn,
            format!(
                "사용자 테마의 template_api 가 {} 이 아니어서 내장 테마를 씁니다: {}",
                giwa_themes::TEMPLATE_API,
                user.display()
            ),
        )
    }
}
