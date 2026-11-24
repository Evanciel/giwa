//! 시험용 외부 모듈 「메아리」. 이벤트·툴·화면·자리·위젯·키-값을 한 번씩 쓴다 (ADR-0177).
//! 크기를 줄이려고 JSON 은 손으로 만든다.

wit_bindgen::generate!({ world: "module", path: "../../wit" });

use giwa::module::host::{self, Level};
use giwa::module::types::Page;

struct Echo;

fn err(code: &str, message: &str) -> ModuleError {
    ModuleError {
        code: code.into(),
        message: message.into(),
    }
}

/// JSON 글자 안에 넣을 수 있게 고친다.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn number(key: &str) -> Result<u64, ModuleError> {
    Ok(host::kv_get(key)?
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0))
}

fn text(key: &str) -> Result<String, ModuleError> {
    Ok(host::kv_get(key)?
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default())
}

/// `{"key":"값"}` 모양의 입력에서 글자 하나를 꺼낸다 (시험용이라 따옴표 안의 이스케이프는 다루지 않는다).
fn field(input: &str, key: &str) -> Option<String> {
    let at = input.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = input[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

impl Guest for Echo {
    fn on_event(ev: Event) -> Result<(), ModuleError> {
        // 같은 이벤트가 다시 와도 한 번만 센다.
        let seen = format!("seen:{}", ev.id);
        if host::kv_get(&seen)?.is_some() {
            return Ok(());
        }
        host::kv_set(&seen, b"1")?;
        let n = number("events")? + 1;
        host::kv_set("events", n.to_string().as_bytes())?;
        host::kv_set("last", ev.action.as_bytes())?;
        host::log(Level::Info, &format!("이벤트 {} 받음", ev.action));
        Ok(())
    }

    fn call_tool(name: String, input: String, actor: String) -> Result<String, ModuleError> {
        match name.as_str() {
            "count" => Ok(format!(
                "{{\"events\":{},\"last\":{},\"settings\":{},\"actor\":{}}}",
                number("events")?,
                quote(&text("last")?),
                host::settings(),
                actor
            )),
            "remember" => {
                let key =
                    field(&input, "key").ok_or_else(|| err("validation", "key 를 적으세요."))?;
                let value = field(&input, "value").unwrap_or_default();
                host::kv_set(&format!("note:{key}"), value.as_bytes())?;
                Ok(format!("{{\"saved\":{}}}", quote(&key)))
            }
            "notes" => {
                let rows = host::kv_list("note:", None, 100)?;
                let items: Vec<String> = rows
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{{\"key\":{},\"value\":{}}}",
                            quote(&k["note:".len()..]),
                            quote(&String::from_utf8_lossy(v))
                        )
                    })
                    .collect();
                Ok(format!("{{\"notes\":[{}]}}", items.join(",")))
            }
            "fail" => Err(err("conflict", "일부러 낸 실패입니다.")),
            "spin" => {
                let mut x: u64 = 0;
                loop {
                    x = core::hint::black_box(x.wrapping_add(1));
                }
            }
            _ => Err(err("not-found", "없는 툴입니다.")),
        }
    }

    fn handle(req: Request) -> Result<Response, ModuleError> {
        let viewer = if req.viewer.contains("\"type\":\"human\"") {
            "회원"
        } else {
            "손님"
        };
        Ok(match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/hello") => Response::Page(Page {
                template: "mod/echo/hello.html".into(),
                title: "메아리".into(),
                data: format!(
                    "{{\"events\":{},\"viewer\":{},\"query\":{}}}",
                    number("events")?,
                    quote(viewer),
                    quote(&req.query)
                ),
            }),
            ("GET", "/json") => Response::Json(format!("{{\"events\":{}}}", number("events")?)),
            ("GET", "/text") => Response::Text(format!("메아리 {}", req.query)),
            ("GET", "/headers") => Response::Text(
                req.headers
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("POST", "/note") if req.same_origin => {
                let body = String::from_utf8_lossy(&req.body).into_owned();
                host::kv_set("note:form", body.as_bytes())?;
                Response::Redirect("/m/echo/hello".into())
            }
            ("POST", "/note") => Response::Text("다른 사이트에서 온 요청".into()),
            ("GET", "/away") => Response::Redirect("//example.com/".into()),
            ("GET", "/raw") => Response::Page(Page {
                template: "theme/index.html".into(),
                title: "남의 템플릿".into(),
                data: "{}".into(),
            }),
            ("GET", "/spin") => {
                let mut x: u64 = 0;
                loop {
                    x = core::hint::black_box(x.wrapping_add(1));
                }
            }
            ("GET", "/eat") => {
                let mut v: Vec<Vec<u8>> = Vec::new();
                loop {
                    v.push(vec![1u8; 1 << 20]);
                    core::hint::black_box(&v);
                }
            }
            _ => Response::NotFound,
        })
    }

    fn widget_view(_kind: String, _config: String) -> Result<String, ModuleError> {
        Ok(format!("{{\"events\":{}}}", number("events")?))
    }

    fn slot(name: String, _view: String) -> Result<Option<SlotContent>, ModuleError> {
        if name != "post_footer" {
            return Ok(None);
        }
        Ok(Some(SlotContent {
            template: "mod/echo/footer.html".into(),
            data: format!("{{\"events\":{}}}", number("events")?),
        }))
    }

    fn check_settings(_settings: String) -> Result<(), ModuleError> {
        Ok(())
    }

    fn purge() -> Result<u64, ModuleError> {
        Ok(0)
    }
}

export!(Echo);
