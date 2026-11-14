//! GAP S8: 외부 공개 (ADR-0118). 실제 바이너리·실제 포트·실제 DB 파일로 터널 없음·서버 없음 오류, 도메인 설정,
//! 임시 터널 수명(가짜 cloudflared), 터널 뒤 방문자 주소를 본다. 진짜 Cloudflare 에는 붙지 않는다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const FAKE_URL: &str = "https://warm-owl-tiny.trycloudflare.com";

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// PATH 에 이것만 둔 채로 실행한다.
fn base(data: &Path, path: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .env("PATH", path)
        .env_remove("GIWA_TOKEN");
    c
}

fn parse(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn ok(data: &Path, path: &Path, args: &[&str]) -> Value {
    let v = parse(base(data, path, args).output().unwrap().stdout);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn fail(data: &Path, path: &Path, args: &[&str]) -> Value {
    let out = base(data, path, args).output().unwrap();
    assert!(!out.status.success(), "실패해야 한다: {args:?}");
    let v = parse(out.stdout);
    assert_eq!(v["ok"], false, "{v}");
    v["error"].clone()
}

/// 임시 주소를 알리고 잠시 도는 가짜 cloudflared.
fn fake_cloudflared(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(windows)]
    std::fs::write(
        dir.join("cloudflared.cmd"),
        format!(
            "@echo off\r\n\
             if \"%1\"==\"--version\" (echo cloudflared version 0.0.0-fake& exit /b 0)\r\n\
             echo 2026 INF Requesting new quick Tunnel on trycloudflare.com... 1>&2\r\n\
             echo 2026 INF ^|  {FAKE_URL}  ^| 1>&2\r\n\
             echo 2026 INF Registered tunnel connection connIndex=0 1>&2\r\n\
             \"%SystemRoot%\\System32\\ping.exe\" -n 30 127.0.0.1 >nul\r\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = dir.join("cloudflared");
        std::fs::write(
            &file,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"--version\" ]; then echo cloudflared version 0.0.0-fake; exit 0; fi\n\
                 echo 'INF |  {FAKE_URL}  |' 1>&2\n\
                 echo 'INF Registered tunnel connection connIndex=0' 1>&2\n\
                 exec sleep 30\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// 곧바로 실패하는 가짜 cloudflared.
fn broken_cloudflared(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(windows)]
    std::fs::write(
        dir.join("cloudflared.cmd"),
        "@echo off\r\necho 2026 ERR failed to request quick Tunnel 1>&2\r\nexit /b 1\r\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = dir.join("cloudflared");
        std::fs::write(
            &file,
            "#!/bin/sh\necho 'ERR failed to request quick Tunnel' 1>&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn start_server(data: &Path) -> (Kill, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve 실행");
    let stdout = child.stdout.take().unwrap();
    let server = Kill(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(line.trim()).expect("serve 시작 JSON");
    (server, v["data"]["bind"].as_str().unwrap().to_owned())
}

fn post_login(bind: &str, visitor: Option<&str>) -> u16 {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("POST /login HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    if let Some(ip) = visitor {
        req.push_str(&format!("CF-Connecting-IP: {ip}\r\n"));
    }
    req.push_str("Content-Type: application/x-www-form-urlencoded\r\nContent-Length: 0\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    String::from_utf8_lossy(&raw)[9..12].parse().unwrap()
}

/// 한 주소의 로그인 칸(분당 10회)이 찰 때까지 보낸다.
fn exhaust(bind: &str, visitor: Option<&str>) {
    for _ in 0..12 {
        post_login(bind, visitor);
    }
    assert_eq!(post_login(bind, visitor), 429, "제한이 걸려야 한다");
}

fn init(tmp: &Path, nothing: &Path) -> PathBuf {
    let data = tmp.join("사이트");
    std::fs::create_dir_all(nothing).unwrap();
    ok(
        &data,
        nothing,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    data
}

#[test]
fn cloudflared_가_없으면_공식_안내를_말하고_아무것도_바꾸지_않는다() {
    let tmp = tempfile::tempdir().unwrap();
    let nothing = tmp.path().join("빈경로");
    let data = init(tmp.path(), &nothing);
    let before = std::fs::read_to_string(data.join("config.toml")).unwrap();

    for args in [
        vec!["expose", "--cloudflare"],
        vec!["expose", "--cloudflare", "--quick"],
        vec!["expose", "--cloudflare", "--hostname", "blog.example.com"],
    ] {
        let e = fail(&data, &nothing, &args);
        let message = e["message"].as_str().unwrap();
        assert!(
            message.contains("developers.cloudflare.com"),
            "공식 안내가 있어야 한다: {e}"
        );
        assert!(e["next"].to_string().contains("expose status"), "{e}");
    }
    assert_eq!(
        std::fs::read_to_string(data.join("config.toml")).unwrap(),
        before
    );

    // 방식을 고르지 않아도 다음 명령을 말한다.
    let e = fail(&data, &nothing, &["expose"]);
    assert!(e["next"].to_string().contains("--cloudflare"), "{e}");

    let status = ok(&data, &nothing, &["expose", "status"]);
    assert!(status["cloudflared"].is_null(), "{status}");
    assert_eq!(status["server_running"], false, "{status}");
    assert!(status["tunnel"].is_null(), "{status}");

    let doctor = base(&data, &nothing, &["doctor"]).output().unwrap().stdout;
    assert!(
        String::from_utf8_lossy(&doctor).contains("\"tunnel\""),
        "진단에 터널 항목이 있어야 한다"
    );
}

#[test]
fn 도메인_공개는_설정을_맞추고_명령_순서를_주며_여러_번_해도_같다() {
    let tmp = tempfile::tempdir().unwrap();
    let nothing = tmp.path().join("빈경로");
    let data = init(tmp.path(), &nothing);
    let fake = tmp.path().join("가짜");
    fake_cloudflared(&fake);

    // PATH 에 짝이 안 맞는 따옴표가 있어도 뒤의 경로에서 찾는다 (실제로 있던 일).
    let sep = if cfg!(windows) { ';' } else { ':' };
    let odd = format!("{}\"{sep}{}", nothing.display(), fake.display());
    let found = ok(&data, Path::new(&odd), &["expose", "status"]);
    assert!(found["cloudflared"]["path"].is_string(), "{found}");

    // 안내만 보는 것은 아무것도 바꾸지 않는다.
    let before = std::fs::read_to_string(data.join("config.toml")).unwrap();
    let guide = ok(&data, &fake, &["expose", "--cloudflare"]);
    assert_eq!(guide["options"].as_array().unwrap().len(), 2, "{guide}");
    assert_eq!(
        guide["cloudflared"]["version"], "cloudflared version 0.0.0-fake",
        "{guide}"
    );
    assert_eq!(
        std::fs::read_to_string(data.join("config.toml")).unwrap(),
        before
    );

    let first = ok(
        &data,
        &fake,
        &[
            "expose",
            "--cloudflare",
            "--hostname",
            "https://Blog.Example.com/",
        ],
    );
    assert_eq!(first["hostname"], "blog.example.com", "{first}");
    assert_eq!(first["site_url"], "https://blog.example.com", "{first}");
    assert_eq!(first["changed"], true, "{first}");
    let steps: Vec<&str> = first["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["command"].as_str().unwrap())
        .collect();
    assert_eq!(steps[0], "cloudflared tunnel login", "{steps:?}");
    assert_eq!(steps[1], "cloudflared tunnel create giwa", "{steps:?}");
    assert_eq!(
        steps[2], "cloudflared tunnel route dns giwa blog.example.com",
        "{steps:?}"
    );
    assert!(steps[3].ends_with(" giwa"), "{steps:?}");

    let config = std::fs::read_to_string(data.join("config.toml")).unwrap();
    assert!(
        config.contains("site_url = \"https://blog.example.com\""),
        "{config}"
    );
    assert!(config.contains("trust_tunnel = true"), "{config}");

    let audit = ok(&data, &fake, &["audit", "list", "--action", "site.expose"]);
    assert_eq!(audit["items"].as_array().unwrap().len(), 1, "{audit}");

    // 다시 해도 쓰지 않고 감사도 늘지 않는다.
    let again = ok(
        &data,
        &fake,
        &["expose", "--cloudflare", "--hostname", "blog.example.com"],
    );
    assert_eq!(again["changed"], false, "{again}");
    let audit = ok(&data, &fake, &["audit", "list", "--action", "site.expose"]);
    assert_eq!(audit["items"].as_array().unwrap().len(), 1, "{audit}");

    // 명령에 그대로 들어가는 값은 좁힌다.
    fail(
        &data,
        &fake,
        &["expose", "--cloudflare", "--hostname", "a.example.com; rm"],
    );
    fail(
        &data,
        &fake,
        &[
            "expose",
            "--cloudflare",
            "--hostname",
            "a.example.com",
            "--tunnel",
            "x y",
        ],
    );
    assert!(
        std::fs::read_to_string(data.join("config.toml"))
            .unwrap()
            .contains("blog.example.com")
    );
}

#[test]
fn 토큰으로는_공개하지_못한다() {
    let tmp = tempfile::tempdir().unwrap();
    let nothing = tmp.path().join("빈경로");
    let data = init(tmp.path(), &nothing);
    let fake = tmp.path().join("가짜");
    fake_cloudflared(&fake);
    let issued = ok(
        &data,
        &nothing,
        &["token", "create", "--name", "넓은", "--scope", "admin"],
    );
    let secret = issued["secret"].as_str().unwrap();

    for args in [
        vec!["expose", "--cloudflare", "--hostname", "blog.example.com"],
        vec!["expose", "--cloudflare", "--quick"],
    ] {
        let out = base(&data, &fake, &args)
            .env("GIWA_TOKEN", secret)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        let v = parse(out.stdout);
        assert_eq!(v["error"]["code"], "FORBIDDEN", "{v}");
    }
    assert!(
        !std::fs::read_to_string(data.join("config.toml"))
            .unwrap()
            .contains("blog.example.com")
    );
}

#[test]
fn 임시_터널은_서버가_있어야_열리고_주소를_알리며_끌_수_있다() {
    let tmp = tempfile::tempdir().unwrap();
    let nothing = tmp.path().join("빈경로");
    let data = init(tmp.path(), &nothing);
    let fake = tmp.path().join("가짜");
    fake_cloudflared(&fake);

    // 서버가 없으면 켜는 명령을 말한다.
    let e = fail(
        &data,
        &fake,
        &["expose", "--cloudflare", "--quick", "--to", "127.0.0.1:9"],
    );
    assert!(e["next"].to_string().contains("giwa serve"), "{e}");
    assert!(data.join("config.toml").exists());

    let (server, bind) = start_server(&data);

    // 터널이 열리지 않으면 cloudflared 가 한 말을 돌려준다.
    let broken = tmp.path().join("고장");
    broken_cloudflared(&broken);
    let e = fail(
        &data,
        &broken,
        &["expose", "--cloudflare", "--quick", "--to", &bind],
    );
    assert!(
        e["message"].as_str().unwrap().contains("failed to request"),
        "{e}"
    );
    assert!(
        ok(&data, &broken, &["expose", "status"])["tunnel"].is_null(),
        "실패하면 기록을 남기지 않는다"
    );

    let mut child = base(
        &data,
        &fake,
        &["expose", "--cloudflare", "--quick", "--to", &bind],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    let stdout = child.stdout.take().unwrap();
    let tunnel = Kill(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let started: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(started["ok"], true, "{started}");
    let d = &started["data"];
    assert_eq!(d["url"], FAKE_URL, "{d}");
    assert_eq!(d["target"], bind.as_str(), "{d}");
    assert_eq!(d["restart_required"], false, "앞의 실패에서 이미 켰다: {d}");
    assert!(d["notes"].to_string().contains("/mcp"), "{d}");

    let status = ok(&data, &fake, &["expose", "status"]);
    assert_eq!(status["tunnel"]["url"], FAKE_URL, "{status}");
    assert_eq!(status["trust_tunnel"], true, "{status}");
    let audit = ok(&data, &fake, &["audit", "list", "--action", "site.expose"]);
    assert!(audit.to_string().contains("quick"), "{audit}");

    // 가짜는 /metrics 를 내지 않으니 끝난 터널로 본다. pid 는 건드리지 않고 기록만 지운다.
    let stopped = ok(&data, &fake, &["expose", "stop"]);
    assert_eq!(stopped["stopped"], false, "{stopped}");
    assert_eq!(stopped["url"], FAKE_URL, "{stopped}");
    assert!(ok(&data, &fake, &["expose", "status"])["tunnel"].is_null());
    // 없어도 성공이다.
    assert_eq!(ok(&data, &fake, &["expose", "stop"])["stopped"], false);
    drop(tunnel);
    drop(server);
}

#[test]
fn 터널_뒤_방문자_주소는_설정을_켠_서버의_루프백_연결에서만_믿는다() {
    let tmp = tempfile::tempdir().unwrap();
    let nothing = tmp.path().join("빈경로");
    let data = init(tmp.path(), &nothing);
    let fake = tmp.path().join("가짜");
    fake_cloudflared(&fake);

    // 기본값: 헤더를 믿지 않는다. 주소를 바꿔 보내도 같은 칸을 쓴다.
    {
        let (_server, bind) = start_server(&data);
        exhaust(&bind, None);
        assert_eq!(post_login(&bind, Some("203.0.113.7")), 429);
    }

    ok(
        &data,
        &fake,
        &["expose", "--cloudflare", "--hostname", "blog.example.com"],
    );

    // 켜면 방문자마다 칸이 따로다. 한 사람이 다 써도 다른 사람은 들어온다.
    let (_server, bind) = start_server(&data);
    exhaust(&bind, Some("203.0.113.7"));
    assert_ne!(post_login(&bind, Some("203.0.113.8")), 429);
    assert_ne!(post_login(&bind, Some("2001:db8::1")), 429);
    // 주소가 아닌 값은 연결 주소로 센다.
    exhaust(&bind, Some("주소 아님"));
    assert_eq!(post_login(&bind, None), 429);
}

const TUNNEL_HOST: &str = "warm-owl-tiny.trycloudflare.com";
const PASSWORD: &str = "test-pass-1234";

/// `cloudflared` 가 넘기는 대로 보낸다: 공개 주소의 `Host`, 방문자 주소. 브라우저 머리글은 `browser` 로 준다.
fn through_tunnel(
    bind: &str,
    method: &str,
    path: &str,
    cookie: &str,
    browser: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {TUNNEL_HOST}\r\nConnection: close\r\n\
         CF-Connecting-IP: 203.0.113.9\r\nX-Forwarded-Proto: https\r\n"
    );
    if !cookie.is_empty() {
        req.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    for (k, v) in browser {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "POST" {
        req.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (
        head[9..12].parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

fn set_cookie(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("set-cookie") {
            return None;
        }
        v.trim()
            .starts_with(&format!("{name}="))
            .then(|| v.trim().to_owned())
    })
}

fn cookie_value(line: &str) -> String {
    line.split(';').next().unwrap().to_owned()
}

fn error_of(page: &str) -> &str {
    let Some(at) = page.find("form-error") else {
        return "(오류 문구 없음)";
    };
    &page[at..(at + 200).min(page.len())]
}

fn csrf_of(body: &str) -> String {
    let at = body.find("name=\"_csrf\" value=\"").unwrap() + 20;
    body[at..at + body[at..].find('"').unwrap()].to_owned()
}

/// 터널 주소로 가입하고 로그인한다. 세션 쿠키 줄을 돌려준다.
fn join_through_tunnel(bind: &str, email: &str) -> String {
    let same = [
        ("Origin", "https://warm-owl-tiny.trycloudflare.com"),
        ("Sec-Fetch-Site", "same-origin"),
    ];
    let (_, head, _) = through_tunnel(bind, "GET", "/signup", "", &[], "");
    let csrf = cookie_value(&set_cookie(&head, "giwa_csrf").expect("CSRF 쿠키"));
    let token = csrf.split_once('=').unwrap().1.to_owned();
    let body = format!(
        "_csrf={token}&email={}&display_name={}&password={PASSWORD}&password2={PASSWORD}",
        email.replace('@', "%40"),
        email.split('@').next().unwrap()
    );
    let (signup, head, signup_page) = through_tunnel(bind, "POST", "/signup", &csrf, &same, &body);
    if let Some(line) = set_cookie(&head, "giwa_session") {
        return line;
    }
    let (_, _, page) = through_tunnel(bind, "GET", "/login", &csrf, &[], "");
    let body = format!(
        "_csrf={}&next=%2F&email={}&password={PASSWORD}",
        csrf_of(&page),
        email.replace('@', "%40")
    );
    let (status, head, _) = through_tunnel(bind, "POST", "/login", &csrf, &same, &body);
    set_cookie(&head, "giwa_session").unwrap_or_else(|| {
        panic!(
            "로그인 실패 {status}, 가입 {signup}: {}",
            error_of(&signup_page)
        )
    })
}

#[test]
fn 터널_주소로_가입하고_로그인하면_회원이_남고_세션이_통한다() {
    let tmp = tempfile::tempdir().unwrap();
    let nothing = tmp.path().join("빈경로");
    let data = init(tmp.path(), &nothing);
    ok(
        &data,
        &nothing,
        &[
            "site",
            "set",
            "--signup",
            "open",
            "--captcha",
            "off",
            "--signup-verify",
            "false",
        ],
    );
    // `--quick` 과 같은 상태: 방문자 주소는 믿고 site_url 은 그대로다.
    let config = data.join("config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("trust_tunnel = true\n{text}")).unwrap();

    {
        let (_server, bind) = start_server(&data);
        let line = join_through_tunnel(&bind, "tunnel-a@example.test");
        let session = cookie_value(&line);
        assert!(
            !line.contains("Secure"),
            "site_url 이 http 면 Secure 가 없다: {line}"
        );
        let (status, _, page) = through_tunnel(&bind, "GET", "/me", &session, &[], "");
        assert_eq!(status, 200, "{page}");
        assert!(page.contains("tunnel-a"), "내 정보가 보여야 한다");
        let members = ok(&data, &nothing, &["member", "list"]);
        assert!(
            members.to_string().contains("tunnel-a@example.test"),
            "{members}"
        );

        // 다른 사이트에서 보낸 POST 는 터널 뒤에서도 막힌다.
        let (status, head, _) = through_tunnel(
            &bind,
            "POST",
            "/login",
            "",
            &[
                ("Origin", "https://evil.example.test"),
                ("Sec-Fetch-Site", "cross-site"),
            ],
            "",
        );
        assert!(set_cookie(&head, "giwa_session").is_none());
        assert_ne!(status, 200);
    }

    // 내 도메인: site_url 이 https 라 세션 쿠키에 Secure 가 붙는다.
    let fake = tmp.path().join("가짜");
    fake_cloudflared(&fake);
    ok(
        &data,
        &fake,
        &["expose", "--cloudflare", "--hostname", "blog.example.com"],
    );
    let (_server, bind) = start_server(&data);
    let line = join_through_tunnel(&bind, "tunnel-b@example.test");
    assert!(line.contains("Secure"), "{line}");
}
