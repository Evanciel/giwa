//! GAP M15: 소셜 로그인 (ADR-0114). 실제 바이너리·실제 포트·실제 DB 파일과, 테스트 안의 가짜 OIDC 공급자로
//! 가입·재로그인·연결·끊기·탈퇴까지 끝까지 본다. 구글·카카오·네이버 주소는 고정이라 응답 읽기는 단위 테스트가 맡는다.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_giwa");
const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");
const SAME: (&str, &str) = ("Sec-Fetch-Site", "same-origin");
const PASSWORD: &str = "test-pass-1234";
const SECRET: &str = "fake-client-secret-abcd";
const GOOD_CODE: &str = "good-code";
const ACCESS_TOKEN: &str = "fake-access-token";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn run(data: &Path, args: &[&str]) -> Value {
    let out = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(args)
        .arg("--json")
        .output()
        .expect("giwa 실행");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 출력");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("JSON 아님: {e}\n{stdout}"))
}

fn giwa(data: &Path, args: &[&str]) -> Value {
    let v = run(data, args);
    assert_eq!(v["ok"], true, "{args:?} → {v}");
    v["data"].clone()
}

fn start_server(data: &Path) -> (Server, String) {
    let mut child = Command::new(BIN)
        .arg("--data")
        .arg(data)
        .args(["serve", "--bind", "127.0.0.1:0", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve 실행");
    let stdout = child.stdout.take().unwrap();
    let server = Server(child);
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(line.trim()).expect("serve 시작 JSON");
    (server, v["data"]["bind"].as_str().unwrap().to_owned())
}

struct Res {
    status: u16,
    head: String,
    body: String,
}

impl Res {
    fn cookie(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if !k.trim().eq_ignore_ascii_case("set-cookie") {
                return None;
            }
            let (n, val) = v.trim().split(';').next()?.split_once('=')?;
            (n == name).then(|| val.to_owned())
        })
    }

    fn location(&self) -> String {
        self.head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("location")
                    .then(|| v.trim().to_owned())
            })
            .unwrap_or_default()
    }

    fn csrf(&self) -> String {
        let at = self.body.find("name=\"_csrf\" value=\"").unwrap() + 20;
        self.body[at..at + self.body[at..].find('"').unwrap()].to_owned()
    }

    /// `action` 폼 안의 CSRF 값.
    fn csrf_of(&self, action: &str) -> String {
        let at = self
            .body
            .find(&format!("action=\"{action}\""))
            .unwrap_or_else(|| panic!("{action} 폼 없음: {}", self.body));
        let at = self.body[at..].find("name=\"_csrf\" value=\"").unwrap() + at + 20;
        self.body[at..at + self.body[at..].find('"').unwrap()].to_owned()
    }
}

fn http(bind: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Res {
    let mut s = TcpStream::connect(bind).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Res {
        status: head[9..12].parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn dec(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(hex, 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn query_value(url: &str, name: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.to_owned())
    })
}

/// 비밀번호로 가입하고 `Cookie` 머리글 값을 돌려준다.
fn signup_and_login(bind: &str, email: &str, name: &str) -> String {
    let page = http(bind, "GET", "/signup", &[], "");
    let pre = page.cookie("giwa_csrf").unwrap();
    let cookie = format!("giwa_csrf={pre}");
    let res = http(
        bind,
        "POST",
        "/signup",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &pre),
            ("email", email),
            ("display_name", name),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ]),
    );
    if let Some(s) = res.cookie("giwa_session") {
        return format!("giwa_session={s}");
    }
    let login = http(bind, "GET", "/login", &[("Cookie", &cookie)], "");
    let res = http(
        bind,
        "POST",
        "/login",
        &[FORM, ("Cookie", &cookie)],
        &form(&[
            ("_csrf", &login.csrf()),
            ("next", "/"),
            ("email", email),
            ("password", PASSWORD),
        ]),
    );
    let session = res
        .cookie("giwa_session")
        .unwrap_or_else(|| panic!("로그인 실패 {}: {}", res.status, res.body));
    format!("giwa_session={session}")
}

/// 가짜 OIDC 공급자. `/authorize` 는 돌아갈 주소로 보내고, `/token` 은 PKCE 값이 있는 코드만 받고,
/// `/userinfo` 는 지금 사용자를 준다.
struct Idp {
    addr: String,
    user: Arc<Mutex<Value>>,
    token_bodies: Arc<Mutex<Vec<String>>>,
}

impl Idp {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let user = Arc::new(Mutex::new(json!({})));
        let token_bodies = Arc::new(Mutex::new(Vec::new()));
        let (u, t) = (Arc::clone(&user), Arc::clone(&token_bodies));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                idp_answer(stream, &u, &t);
            }
        });
        Self {
            addr,
            user,
            token_bodies,
        }
    }

    fn set_user(&self, sub: &str, email: &str, name: &str) {
        *self.user.lock().unwrap() = json!({ "sub": sub, "email": email, "name": name });
    }

    fn last_token_body(&self) -> String {
        self.token_bodies.lock().unwrap().last().unwrap().clone()
    }
}

fn idp_answer(mut stream: TcpStream, user: &Mutex<Value>, tokens: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or_default().to_owned(),
        parts.next().unwrap_or_default().to_owned(),
    );
    let (mut length, mut bearer) = (0_usize, String::new());
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else {
            continue;
        };
        if k.eq_ignore_ascii_case("content-length") {
            length = v.trim().parse().unwrap();
        } else if k.eq_ignore_ascii_case("authorization") {
            bearer = v.trim().trim_start_matches("Bearer ").to_owned();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body = String::from_utf8(body).unwrap();

    let path = target.split('?').next().unwrap_or_default();
    let (status, extra, payload) = match (method.as_str(), path) {
        ("GET", "/authorize") => {
            let back = dec(&query_value(&target, "redirect_uri").unwrap());
            let state = query_value(&target, "state").unwrap();
            (
                "302 Found",
                format!("Location: {back}?code={GOOD_CODE}&state={state}\r\n"),
                String::new(),
            )
        }
        ("POST", "/token") => {
            tokens.lock().unwrap().push(body.clone());
            let good = query_value(&format!("?{body}"), "code").as_deref() == Some(GOOD_CODE)
                && query_value(&format!("?{body}"), "code_verifier").is_some();
            if good {
                (
                    "200 OK",
                    String::new(),
                    json!({ "access_token": ACCESS_TOKEN, "token_type": "bearer" }).to_string(),
                )
            } else {
                (
                    "400 Bad Request",
                    String::new(),
                    json!({ "error": "invalid_grant" }).to_string(),
                )
            }
        }
        ("GET", "/userinfo") if bearer == ACCESS_TOKEN => {
            ("200 OK", String::new(), user.lock().unwrap().to_string())
        }
        _ => ("401 Unauthorized", String::new(), "{}".to_owned()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\n{extra}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
}

/// 공급자에 다녀온 뒤: state 쿠키 값과 돌아올 주소(경로만).
struct Flow {
    cookie: String,
    callback: String,
}

/// `/start` → 가짜 공급자 → 돌아올 주소까지. init 의 site_url 은 8080 이라 경로만 떼어 실제 포트로 보낸다.
fn begin(bind: &str, idp: &Idp, query: &str, session: &str) -> Flow {
    let path = format!("/m/social/oidc/start{query}");
    let start = http(bind, "GET", &path, &[("Cookie", session)], "");
    assert_eq!(start.status, 303, "{}", start.body);
    let to_idp = start.location();
    let prefix = format!("http://{}", idp.addr);
    assert!(to_idp.starts_with(&prefix), "{to_idp}");
    let cookie = start.cookie("giwa_social").expect("state 쿠키");
    let back = http(&idp.addr, "GET", &to_idp[prefix.len()..], &[], "");
    assert_eq!(back.status, 302, "{}", back.body);
    let back = back.location();
    let callback = back
        .strip_prefix("http://127.0.0.1:8080")
        .unwrap_or_else(|| panic!("돌아올 주소는 site_url 위에: {back}"))
        .to_owned();
    Flow { cookie, callback }
}

/// 공급자에서 돌아온다 (브라우저처럼 state 쿠키와 로그인 쿠키를 함께 보낸다).
fn finish(bind: &str, flow: &Flow, session: &str) -> Res {
    let cookie = if session.is_empty() {
        format!("giwa_social={}", flow.cookie)
    } else {
        format!("giwa_social={}; {session}", flow.cookie)
    };
    http(bind, "GET", &flow.callback, &[("Cookie", &cookie)], "")
}

/// 새 공급자 계정으로 가입 화면까지 가서 가입하고 `Cookie` 머리글 값을 돌려준다.
fn join_as(bind: &str, idp: &Idp, sub: &str, email: &str, name: &str) -> String {
    idp.set_user(sub, email, name);
    let back = finish(bind, &begin(bind, idp, "", ""), "");
    assert_eq!(back.status, 303, "{}", back.body);
    assert_eq!(back.location(), "/m/social/join");
    let join = format!(
        "giwa_social_join={}",
        back.cookie("giwa_social_join").unwrap()
    );
    let res = http(
        bind,
        "POST",
        "/m/social/join",
        &[FORM, SAME, ("Cookie", &join)],
        &form(&[("email", email), ("display_name", name)]),
    );
    let session = res
        .cookie("giwa_session")
        .unwrap_or_else(|| panic!("가입 뒤 세션 없음 {}: {}", res.status, res.body));
    format!("giwa_session={session}")
}

fn member_id(data: &Path, email: &str) -> String {
    let list = giwa(data, &["member", "list"]);
    let found = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["email"] == email)
        .unwrap_or_else(|| panic!("{email} 없음: {list}"));
    found["id"].as_str().unwrap().to_owned()
}

fn identities(data: &Path, member: &str) -> Vec<Value> {
    giwa(data, &["social", "identities", "--member", member])["items"]
        .as_array()
        .unwrap()
        .clone()
}

fn set_oidc(data: &Path, idp: &Idp) -> Value {
    let (auth, token, userinfo) = (
        format!("http://{}/authorize", idp.addr),
        format!("http://{}/token", idp.addr),
        format!("http://{}/userinfo", idp.addr),
    );
    giwa(
        data,
        &[
            "social",
            "provider_set",
            "--provider",
            "oidc",
            "--client-id",
            "maru-test",
            "--client-secret",
            SECRET,
            "--label",
            "사내",
            "--auth-url",
            &auth,
            "--token-url",
            &token,
            "--userinfo-url",
            &userinfo,
        ],
    )
}

#[test]
fn social_login_signup_link_unlink_withdraw() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("사이트");
    giwa(
        &data,
        &["init", "--no-samples", "--preset", "blog", "--yes"],
    );
    giwa(
        &data,
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
    giwa(&data, &["module", "enable", "social"]);
    let idp = Idp::start();
    let (server, bind) = start_server(&data);

    // 공급자가 없으면 단추도 없다.
    assert!(
        !http(&bind, "GET", "/login", &[], "")
            .body
            .contains("계정으로 계속하기")
    );
    assert_eq!(
        http(&bind, "GET", "/m/social/oidc/start", &[], "").status,
        404
    );

    // 공급자 넣기: 바깥으로 나가는 http 주소·고정 공급자의 주소는 받지 않는다.
    let far = run(
        &data,
        &[
            "social",
            "provider_set",
            "--provider",
            "oidc",
            "--client-id",
            "maru-test",
            "--auth-url",
            "http://203.0.113.9/authorize",
            "--token-url",
            "https://idp.example.test/token",
            "--userinfo-url",
            "https://idp.example.test/userinfo",
        ],
    );
    assert_eq!(far["error"]["code"], "VALIDATION", "{far}");
    let fixed = run(
        &data,
        &[
            "social",
            "provider_set",
            "--provider",
            "google",
            "--client-id",
            "maru-test",
            "--auth-url",
            "https://idp.example.test/authorize",
        ],
    );
    assert_eq!(fixed["error"]["code"], "VALIDATION", "{fixed}");
    set_oidc(&data, &idp);

    // 목록에 비밀값 원문은 없고 끝 네 글자와 돌아올 주소가 있다.
    let list = giwa(&data, &["social", "provider_list"]);
    assert!(!list.to_string().contains(SECRET), "{list}");
    let item = &list["items"][0];
    assert_eq!(item["provider"], "oidc");
    assert_eq!(item["secret_hint"], "…abcd");
    assert_eq!(item["usable"], true);
    assert_eq!(
        item["redirect_uri"],
        "http://127.0.0.1:8080/m/social/oidc/callback"
    );

    // 로그인·가입 화면의 단추. 돌아갈 곳이 따라간다.
    let login = http(&bind, "GET", "/login", &[], "");
    assert!(
        login.body.contains("사내 계정으로 계속하기"),
        "{}",
        login.body
    );
    assert!(login.body.contains("href=\"/m/social/oidc/start"));
    assert!(
        http(&bind, "GET", "/signup", &[], "")
            .body
            .contains("사내 계정으로 계속하기")
    );
    assert!(
        http(&bind, "GET", "/login?next=/b/free", &[], "")
            .body
            .contains("/m/social/oidc/start?next="),
    );

    // 시작: state 쿠키와 PKCE·nonce. 비밀값은 주소에 없다.
    let start = http(&bind, "GET", "/m/social/oidc/start", &[], "");
    assert_eq!(start.status, 303, "{}", start.body);
    let to_idp = start.location();
    assert_eq!(
        query_value(&to_idp, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        query_value(&to_idp, "code_challenge").map(|c| c.len()),
        Some(43)
    );
    assert!(query_value(&to_idp, "nonce").is_some());
    assert_eq!(
        query_value(&to_idp, "client_id").as_deref(),
        Some("maru-test")
    );
    assert!(!to_idp.contains(SECRET));
    let set_cookie = start
        .head
        .lines()
        .find(|l| {
            l.to_ascii_lowercase()
                .starts_with("set-cookie: giwa_social=")
        })
        .unwrap();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/m/social"), "{set_cookie}");
    // 원문은 쿠키에만 — 쿠키와 공급자로 보낸 state 가 같다.
    assert_eq!(query_value(&to_idp, "state"), start.cookie("giwa_social"));

    // 다른 브라우저(쿠키 없음·다른 쿠키)에서 돌아오면 받지 않는다.
    idp.set_user("sub-sora", "sora@example.test", "소라");
    let flow = begin(&bind, &idp, "", "");
    assert_eq!(http(&bind, "GET", &flow.callback, &[], "").status, 401);
    assert_eq!(
        http(
            &bind,
            "GET",
            &flow.callback,
            &[("Cookie", "giwa_social=other")],
            ""
        )
        .status,
        401
    );

    // 쿠키가 맞으면 코드를 바꿔 가입 화면으로. 토큰 요청에 PKCE·비밀값·돌아올 주소가 실린다.
    let back = finish(&bind, &flow, "");
    assert_eq!(back.status, 303, "{}", back.body);
    assert_eq!(back.location(), "/m/social/join");
    let body = idp.last_token_body();
    assert!(body.contains("code_verifier="), "{body}");
    assert!(body.contains(&format!("client_secret={SECRET}")), "{body}");
    assert!(body.contains("grant_type=authorization_code"), "{body}");
    assert_eq!(
        dec(&query_value(&format!("?{body}"), "redirect_uri").unwrap()),
        "http://127.0.0.1:8080/m/social/oidc/callback"
    );
    let join_cookie = format!(
        "giwa_social_join={}",
        back.cookie("giwa_social_join").unwrap()
    );

    // state 는 한 번만 쓴다.
    assert_eq!(finish(&bind, &flow, "").status, 401);

    // 취소하고 돌아오거나 공급자가 코드를 안 받아 주면 로그인되지 않는다.
    let cancel = begin(&bind, &idp, "", "");
    let state = query_value(&cancel.callback, "state").unwrap();
    let denied = Flow {
        cookie: cancel.cookie,
        callback: format!("/m/social/oidc/callback?error=access_denied&state={state}"),
    };
    let res = finish(&bind, &denied, "");
    assert_eq!(res.status, 401, "{}", res.body);
    assert!(res.cookie("giwa_session").is_none());
    let bad = begin(&bind, &idp, "", "");
    let state = query_value(&bad.callback, "state").unwrap();
    let bad = Flow {
        cookie: bad.cookie,
        callback: format!("/m/social/oidc/callback?code=wrong&state={state}"),
    };
    let res = finish(&bind, &bad, "");
    assert_eq!(res.status, 401, "{}", res.body);
    assert!(res.cookie("giwa_session").is_none());

    // 가입 화면은 공급자가 준 이메일·이름을 미리 채운다. 쿠키 없이는 못 본다.
    assert_eq!(http(&bind, "GET", "/m/social/join", &[], "").status, 401);
    let page = http(
        &bind,
        "GET",
        "/m/social/join",
        &[("Cookie", &join_cookie)],
        "",
    );
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(
        page.body.contains("value=\"sora@example.test\""),
        "{}",
        page.body
    );
    assert!(page.body.contains("value=\"소라\""), "{}", page.body);

    // 다른 사이트에서 보낸 폼은 막는다. 닉네임이 맞지 않으면 화면에 알리고 흐름은 살아 있다.
    let refused = http(
        &bind,
        "POST",
        "/m/social/join",
        &[
            FORM,
            ("Sec-Fetch-Site", "cross-site"),
            ("Cookie", &join_cookie),
        ],
        &form(&[("email", "sora@example.test"), ("display_name", "소라")]),
    );
    assert_eq!(refused.status, 403);
    let short = http(
        &bind,
        "POST",
        "/m/social/join",
        &[FORM, SAME, ("Cookie", &join_cookie)],
        &form(&[("email", "sora@example.test"), ("display_name", "소")]),
    );
    assert!(short.body.contains("form-error"), "{}", short.body);
    assert!(short.cookie("giwa_session").is_none());

    // 가입하면 바로 로그인된다.
    let done = http(
        &bind,
        "POST",
        "/m/social/join",
        &[FORM, SAME, ("Cookie", &join_cookie)],
        &form(&[("email", "sora@example.test"), ("display_name", "소라")]),
    );
    assert_eq!(done.status, 303, "{}", done.body);
    assert_eq!(done.location(), "/me");
    let sora = format!("giwa_session={}", done.cookie("giwa_session").unwrap());
    let sora_id = member_id(&data, "sora@example.test");
    let linked = identities(&data, &sora_id);
    assert_eq!(linked.len(), 1, "{linked:?}");
    assert_eq!(linked[0]["provider"], "oidc");
    assert_eq!(linked[0]["email"], "sora@example.test");
    // 가입 흐름도 한 번만 쓴다.
    assert_eq!(
        http(
            &bind,
            "POST",
            "/m/social/join",
            &[FORM, SAME, ("Cookie", &join_cookie)],
            &form(&[("email", "sora2@example.test"), ("display_name", "소라둘")]),
        )
        .status,
        401
    );

    // 내 정보: 소셜 칸이 있고, 비밀번호가 없으니 「지금 비밀번호」 칸은 없다.
    let me = http(&bind, "GET", "/me", &[("Cookie", &sora)], "");
    assert_eq!(me.status, 200, "{}", me.body);
    assert!(me.body.contains("id=\"social\""), "{}", me.body);
    assert!(me.body.contains("연결 끊기"), "{}", me.body);
    assert!(!me.body.contains("name=\"current\""), "{}", me.body);
    assert!(me.body.contains("비밀번호가 없습니다"), "{}", me.body);
    // 닉네임 입력으로 탈퇴를 확인한다.
    assert!(me.body.contains("지금 닉네임"), "{}", me.body);

    // 다시 로그인하면 가입 화면 없이 바로 세션. 돌아갈 곳은 사이트 안만.
    let again = finish(&bind, &begin(&bind, &idp, "?next=/b/free", ""), "");
    assert_eq!(again.status, 303, "{}", again.body);
    assert_eq!(again.location(), "/b/free");
    assert!(again.cookie("giwa_session").is_some());
    let evil = finish(
        &bind,
        &begin(&bind, &idp, "?next=https://evil.example.test/", ""),
        "",
    );
    assert_eq!(evil.location(), "/me");
    let evil = finish(
        &bind,
        &begin(&bind, &idp, "?next=//evil.example.test/", ""),
        "",
    );
    assert_eq!(evil.location(), "/me");
    assert_eq!(
        identities(&data, &sora_id).len(),
        1,
        "새 연결이 생기지 않는다"
    );

    // 비밀번호 회원이 내 정보에서 연결한다. 로그인 안 했으면 로그인 화면으로 보낸다.
    let guest = http(&bind, "GET", "/m/social/oidc/start?link=1", &[], "");
    assert_eq!(guest.status, 303);
    assert!(
        guest.location().starts_with("/login"),
        "{}",
        guest.location()
    );
    let moon = signup_and_login(&bind, "moon@example.test", "달님");
    let moon_id = member_id(&data, "moon@example.test");
    let me = http(&bind, "GET", "/me", &[("Cookie", &moon)], "");
    assert!(
        me.body.contains("name=\"current\""),
        "비밀번호 회원: {}",
        me.body
    );
    assert!(
        me.body.contains("/m/social/oidc/start?link=1"),
        "{}",
        me.body
    );

    idp.set_user("sub-moon", "moon-other@example.test", "달");
    let link = begin(&bind, &idp, "?link=1", &moon);
    // 연결을 시작한 회원이 아니면 받지 않는다.
    let other = signup_and_login(&bind, "star@example.test", "별님");
    assert_eq!(finish(&bind, &link, &other).status, 401);
    let res = finish(&bind, &begin(&bind, &idp, "?link=1", &moon), &moon);
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/me#social");
    let linked = identities(&data, &moon_id);
    assert_eq!(linked.len(), 1, "{linked:?}");
    assert_eq!(linked[0]["email"], "moon-other@example.test");
    let me = http(&bind, "GET", "/me", &[("Cookie", &moon)], "");
    assert!(me.body.contains("moon-other@example.test"), "{}", me.body);
    // 같은 계정을 다시 연결해도 그대로다.
    assert_eq!(
        finish(&bind, &begin(&bind, &idp, "?link=1", &moon), &moon).status,
        303
    );
    assert_eq!(identities(&data, &moon_id).len(), 1);
    // 연결된 공급자 계정으로 로그인하면 그 회원이다.
    let res = finish(&bind, &begin(&bind, &idp, "", ""), "");
    assert_eq!(res.status, 303, "{}", res.body);
    let via_link = format!("giwa_session={}", res.cookie("giwa_session").unwrap());
    assert!(
        http(&bind, "GET", "/me", &[("Cookie", &via_link)], "")
            .body
            .contains("달님")
    );

    // 다른 회원에 연결된 공급자 계정은 가져올 수 없다.
    idp.set_user("sub-sora", "sora@example.test", "소라");
    assert_eq!(
        finish(&bind, &begin(&bind, &idp, "?link=1", &other), &other).status,
        409
    );
    // 공급자마다 계정은 하나: 이미 연결했으면 다른 계정은 먼저 끊어야 한다.
    idp.set_user("sub-moon-2", "moon-2@example.test", "달둘");
    assert_eq!(
        finish(&bind, &begin(&bind, &idp, "?link=1", &moon), &moon).status,
        409
    );
    assert_eq!(identities(&data, &moon_id).len(), 1);

    // 끊기: 다른 사이트 폼은 막고, 비밀번호 없는 회원의 마지막 연결은 못 끊고, 비밀번호 회원은 끊는다.
    let unlink = "/m/social/unlink/oidc";
    assert_eq!(
        http(
            &bind,
            "POST",
            unlink,
            &[("Cookie", &moon), ("Sec-Fetch-Site", "cross-site")],
            ""
        )
        .status,
        403
    );
    assert_eq!(identities(&data, &moon_id).len(), 1);
    let res = http(&bind, "POST", unlink, &[SAME, ("Cookie", &sora)], "");
    assert_eq!(res.status, 409, "{}", res.body);
    assert_eq!(identities(&data, &sora_id).len(), 1, "마지막 연결은 남는다");
    let res = http(&bind, "POST", unlink, &[SAME, ("Cookie", &moon)], "");
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/me#social");
    assert!(identities(&data, &moon_id).is_empty());
    assert_eq!(
        http(&bind, "POST", unlink, &[SAME, ("Cookie", &moon)], "").status,
        404,
        "연결이 없으면 끊을 것도 없다"
    );

    // 믿지 않는 공급자(oidc)의 이메일이 기존 회원과 같으면 연결하지도 가입시키지도 않는다.
    idp.set_user("sub-fake-moon", "moon@example.test", "가짜달");
    let res = finish(&bind, &begin(&bind, &idp, "", ""), "");
    assert_eq!(res.status, 409, "{}", res.body);
    assert!(res.cookie("giwa_session").is_none());
    assert!(identities(&data, &moon_id).is_empty());
    // 가입 화면에서 기존 이메일로 바꿔 적어도 마찬가지다.
    idp.set_user("sub-sneak", "sneak@example.test", "몰래");
    let back = finish(&bind, &begin(&bind, &idp, "", ""), "");
    let cookie = format!(
        "giwa_social_join={}",
        back.cookie("giwa_social_join").unwrap()
    );
    let res = http(
        &bind,
        "POST",
        "/m/social/join",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&[("email", "moon@example.test"), ("display_name", "몰래")]),
    );
    assert!(res.body.contains("form-error"), "{}", res.body);
    assert!(res.cookie("giwa_session").is_none());
    assert!(identities(&data, &moon_id).is_empty());

    // 비밀번호 없는 회원이 비밀번호를 정하면 이메일로도 로그인하고 마지막 연결도 끊을 수 있다.
    let hana = join_as(&bind, &idp, "sub-hana", "hana@example.test", "하나");
    let hana_id = member_id(&data, "hana@example.test");
    let me = http(&bind, "GET", "/me", &[("Cookie", &hana)], "");
    let res = http(
        &bind,
        "POST",
        "/me/password",
        &[FORM, ("Cookie", &hana)],
        &form(&[
            ("_csrf", &me.csrf_of("/me/password")),
            ("password", PASSWORD),
            ("password2", PASSWORD),
        ]),
    );
    assert_eq!(res.status, 303, "{}", res.body);
    let hana = format!("giwa_session={}", res.cookie("giwa_session").unwrap());
    let me = http(&bind, "GET", "/me", &[("Cookie", &hana)], "");
    assert!(me.body.contains("name=\"current\""), "{}", me.body);
    let res = http(&bind, "POST", unlink, &[SAME, ("Cookie", &hana)], "");
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(identities(&data, &hana_id).is_empty());

    // 비밀번호 없는 회원은 닉네임으로 탈퇴를 확인한다. 연결도 사라지고, 같은 계정으로 새로 가입할 수 있다.
    let me = http(&bind, "GET", "/me", &[("Cookie", &sora)], "");
    let res = http(
        &bind,
        "POST",
        "/me/withdraw",
        &[FORM, ("Cookie", &sora)],
        &form(&[
            ("_csrf", &me.csrf_of("/me/withdraw")),
            ("password", "다른이름"),
            ("agree", "yes"),
        ]),
    );
    assert!(res.body.contains("닉네임을 그대로"), "{}", res.body);
    assert_eq!(identities(&data, &sora_id).len(), 1);
    let me = http(&bind, "GET", "/me", &[("Cookie", &sora)], "");
    let res = http(
        &bind,
        "POST",
        "/me/withdraw",
        &[FORM, ("Cookie", &sora)],
        &form(&[
            ("_csrf", &me.csrf_of("/me/withdraw")),
            ("password", "소라"),
            ("agree", "yes"),
        ]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("탈퇴했습니다"), "{}", res.body);
    let wait = std::time::Instant::now();
    while !identities(&data, &sora_id).is_empty() {
        assert!(wait.elapsed().as_secs() < 15, "탈퇴 뒤에도 연결이 남았다");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    idp.set_user("sub-sora", "sora@example.test", "소라");
    let res = finish(&bind, &begin(&bind, &idp, "", ""), "");
    assert_eq!(res.status, 303, "{}", res.body);
    assert_eq!(res.location(), "/m/social/join");
    drop(server);

    // 메일 인증이 켜진 사이트: oidc 이메일은 믿지 않으므로 메일을 확인해야 로그인된다.
    giwa(&data, &["site", "set", "--signup-verify", "true"]);
    let (server, bind) = start_server(&data);
    idp.set_user("sub-mail", "mail@example.test", "메일");
    let back = finish(&bind, &begin(&bind, &idp, "", ""), "");
    let cookie = format!(
        "giwa_social_join={}",
        back.cookie("giwa_social_join").unwrap()
    );
    let res = http(
        &bind,
        "POST",
        "/m/social/join",
        &[FORM, SAME, ("Cookie", &cookie)],
        &form(&[("email", "mail@example.test"), ("display_name", "메일")]),
    );
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(res.body.contains("메일을 확인하세요"), "{}", res.body);
    assert!(res.cookie("giwa_session").is_none());
    let res = finish(&bind, &begin(&bind, &idp, "", ""), "");
    assert_eq!(res.status, 403, "{}", res.body);
    assert!(res.cookie("giwa_session").is_none());
    drop(server);

    // 가입을 닫으면 새 계정은 못 들어오지만 연결된 회원은 그대로 로그인한다.
    giwa(
        &data,
        &["site", "set", "--signup", "off", "--signup-verify", "false"],
    );
    let (_server, bind) = start_server(&data);
    idp.set_user("sub-late", "late@example.test", "늦은이");
    let res = finish(&bind, &begin(&bind, &idp, "", ""), "");
    assert_eq!(res.status, 403, "{}", res.body);
    assert!(res.cookie("giwa_social_join").is_none());
    idp.set_user("sub-mail", "mail@example.test", "메일");
    let res = finish(&bind, &begin(&bind, &idp, "", ""), "");
    assert_eq!(res.status, 303, "{}", res.body);
    assert!(res.cookie("giwa_session").is_some());

    // 공급자를 다시 넣어도 하나이고, 빼면 단추가 사라지되 연결은 남는다.
    set_oidc(&data, &idp);
    assert_eq!(
        giwa(&data, &["social", "provider_list"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    giwa(&data, &["social", "provider_remove", "--provider", "oidc"]);
    assert!(
        !http(&bind, "GET", "/login", &[], "")
            .body
            .contains("계정으로 계속하기")
    );
    let mail_id = member_id(&data, "mail@example.test");
    assert_eq!(identities(&data, &mail_id).len(), 1);
}
