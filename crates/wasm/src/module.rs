//! 외부 모듈 하나를 [`Module`] 로 감싼다 (ADR-0177).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State as AxState;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::any;
use axum::{Extension, Router};
use giwa_core::actor::{Actor, Permission};
use giwa_core::fields::FieldDef;
use giwa_core::model::AuditEvent;
use giwa_core::services::widgets::WidgetType;
use giwa_core::store::ModuleMigration;
use giwa_core::{Code, Ctx, Error, Result};
use giwa_modules::service::settings_of;
use giwa_modules::{
    Manifest, Module, ModuleState, SlotContent, SlotView, ToolDef, module_error, tool_not_found,
};
use giwa_storage::Db;
use serde_json::{Map, Value, json};
use tokio::sync::OnceCell;

use crate::bindings::ModulePre;
use crate::bindings::giwa::module::types;
use crate::kv;
use crate::manifest::{ModuleToml, bad, unsafe_template};
use crate::runtime::{Budget, Fault, MAX_OUTPUT, State, runtime, short};

/// 패키지 안 템플릿을 모두 합친 크기
const MAX_TEMPLATES: usize = 1_000_000;
/// 요청 본문
const MAX_BODY: usize = 1_000_000;
/// 모듈에 넘기지 않는 요청 머리글 (로그인 정보)
const HIDDEN_HEADERS: [&str; 3] = ["cookie", "authorization", "proxy-authorization"];

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[derive(Clone)]
pub struct ExternalModule {
    inner: Arc<Inner>,
}

struct Inner {
    manifest: &'static Manifest,
    wasm: PathBuf,
    db: Db,
    tools: Vec<ToolDef>,
    templates: Vec<(&'static str, &'static str)>,
    settings_fields: Vec<FieldDef>,
    defaults: Map<String, Value>,
    widgets: Vec<WidgetType>,
    slots: Vec<String>,
    cleanup: &'static str,
    pre: OnceCell<std::result::Result<ModulePre<State>, String>>,
}

impl ExternalModule {
    /// 폴더 하나를 읽는다. 컴파일은 처음 부를 때 한다.
    pub fn open(dir: &Path, db: Db) -> Result<Self> {
        let toml_path = dir.join("module.toml");
        let text =
            std::fs::read_to_string(&toml_path).map_err(|e| bad(&toml_path, e.to_string()))?;
        let m = ModuleToml::parse(&toml_path, &text)?;
        let id = m.module.id.clone();
        if dir.file_name().and_then(|n| n.to_str()) != Some(id.as_str()) {
            return Err(bad(
                &toml_path,
                format!("폴더 이름이 id({id})와 같아야 합니다"),
            ));
        }
        let wasm = dir.join("module.wasm");
        if !wasm.is_file() {
            return Err(bad(&wasm, "module.wasm 이 없습니다"));
        }
        let templates = read_templates(&dir.join("templates"), &id)?;
        let id: &'static str = leak(id);
        let events: Vec<&'static str> = m.module.events.into_iter().map(leak).collect();
        let manifest: &'static Manifest = Box::leak(Box::new(Manifest {
            id,
            name: leak(m.module.name),
            version: leak(m.module.version),
            description: leak(m.module.description),
            events: Box::leak(events.into_boxed_slice()),
        }));
        let tools = m
            .tools
            .into_iter()
            .map(|t| ToolDef {
                input_schema: ModuleToml::schema(&t.input_schema),
                name: leak(t.name),
                description: leak(t.description),
                permission: t.permission.as_deref().and_then(Permission::parse),
            })
            .collect();
        let widgets = m
            .widgets
            .into_iter()
            .map(|w| WidgetType {
                config_schema: ModuleToml::schema(&w.config_schema),
                id: leak(w.id),
                name: leak(w.name),
                description: leak(w.description),
            })
            .collect();
        Ok(Self {
            inner: Arc::new(Inner {
                manifest,
                wasm,
                db,
                tools,
                templates,
                settings_fields: m.settings,
                defaults: m.defaults,
                widgets,
                slots: m.module.slots,
                cleanup: leak(kv::cleanup_sql(id)),
                pre: OnceCell::new(),
            }),
        })
    }

    pub fn id(&self) -> &'static str {
        self.inner.manifest.id
    }

    fn template(&self, name: &str) -> Option<&'static str> {
        self.inner
            .templates
            .iter()
            .map(|(n, _)| *n)
            .find(|n| *n == name)
    }

    fn fail(&self, f: Fault) -> Error {
        match f {
            Fault::Guest(e) => {
                let code = match e.code.as_str() {
                    "validation" => Code::Validation,
                    "not-found" => Code::NotFound,
                    "forbidden" => Code::Forbidden,
                    "conflict" => Code::Conflict,
                    _ => Code::Internal,
                };
                let text: String = e.message.chars().take(500).collect();
                module_error(code, text)
            }
            Fault::Host(detail) => {
                tracing::warn!(module = self.id(), error = %detail, "외부 모듈 실행 실패");
                module_error(
                    Code::Internal,
                    format!("외부 모듈({})이 일을 마치지 못했습니다.", self.id()),
                )
            }
        }
    }

    async fn pre(&self) -> Result<&ModulePre<State>> {
        let rt = runtime().map_err(|f| self.fail(f))?;
        let wasm = self.inner.wasm.clone();
        let id = self.id();
        let got = self
            .inner
            .pre
            .get_or_init(|| async move {
                let started = std::time::Instant::now();
                let done = tokio::task::spawn_blocking(move || rt.prepare(&wasm))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                tracing::info!(
                    module = id,
                    ms = started.elapsed().as_millis() as u64,
                    ok = done.is_ok(),
                    "외부 모듈 준비"
                );
                done
            })
            .await;
        got.as_ref().map_err(|e| self.fail(Fault::Host(e.clone())))
    }

    /// 한 번 부른다. `f` 가 띄운 인스턴스의 내보낸 함수를 부른다.
    async fn run<T, F, Fut>(&self, ctx: &Ctx, budget: Budget, f: F) -> Result<T>
    where
        F: FnOnce(crate::runtime::Call) -> Fut,
        Fut: Future<Output = wasmtime::Result<std::result::Result<T, types::ModuleError>>>,
    {
        let rt = runtime().map_err(|f| self.fail(f))?;
        let pre = self.pre().await?;
        let settings = Value::Object(settings_of(ctx, self).await?).to_string();
        let state = State::new(self.id(), self.inner.db.clone(), settings);
        let call = rt
            .instance(pre, state, budget)
            .await
            .map_err(|f| self.fail(f))?;
        let deadline: Duration = call.deadline();
        match tokio::time::timeout(deadline, f(call)).await {
            Err(_) => Err(self.fail(Fault::Host(format!(
                "{}초 안에 끝나지 않았습니다",
                deadline.as_secs()
            )))),
            Ok(Err(e)) => Err(self.fail(Fault::Host(short(&e)))),
            Ok(Ok(Err(e))) => Err(self.fail(Fault::Guest(e))),
            Ok(Ok(Ok(v))) => Ok(v),
        }
    }

    fn json_of(&self, text: &str) -> Result<Value> {
        if text.len() > MAX_OUTPUT {
            return Err(self.fail(Fault::Host(format!(
                "응답이 {}바이트를 넘습니다",
                MAX_OUTPUT
            ))));
        }
        serde_json::from_str(text)
            .map_err(|e| self.fail(Fault::Host(format!("JSON 이 아닌 응답: {e}"))))
    }

    async fn handle(&self, state: &ModuleState, req: Request) -> Response {
        let viewer = match state.host.viewer(&req.headers).await {
            Ok(a) => a,
            Err(e) => return state.host.error(&e).await,
        };
        if req.body.len() > MAX_BODY {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
        let wit = types::Request {
            method: req.method.as_str().to_owned(),
            path: req.uri.path().to_owned(),
            query: req.uri.query().unwrap_or("").to_owned(),
            headers: req
                .headers
                .iter()
                .filter(|(k, _)| !HIDDEN_HEADERS.contains(&k.as_str()))
                .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
                .collect(),
            body: req.body.to_vec(),
            viewer: actor_json(&viewer).to_string(),
            same_origin: state.host.same_origin(&req.headers),
        };
        let res = self
            .run(&state.ctx, Budget::Screen, |mut c| async move {
                c.guest.call_handle(&mut c.store, &wit).await
            })
            .await;
        let res = match res {
            Ok(r) => r,
            Err(e) => return state.host.error(&e).await,
        };
        match res {
            types::Response::Page(p) => {
                let Some(template) = self.template(&p.template) else {
                    let e = self.fail(Fault::Host(format!("패키지에 없는 템플릿: {}", p.template)));
                    return state.host.error(&e).await;
                };
                match self.json_of(&p.data) {
                    Ok(data @ Value::Object(_)) => state.host.page(template, &p.title, data).await,
                    Ok(_) => {
                        let e = self.fail(Fault::Host("화면 값이 JSON 객체가 아닙니다".into()));
                        state.host.error(&e).await
                    }
                    Err(e) => state.host.error(&e).await,
                }
            }
            types::Response::Json(text) => match self.json_of(&text) {
                Ok(_) => ([(header::CONTENT_TYPE, "application/json")], text).into_response(),
                Err(e) => state.host.error(&e).await,
            },
            types::Response::Text(text) if text.len() <= MAX_OUTPUT => {
                ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
            }
            types::Response::Text(_) => {
                let e = self.fail(Fault::Host("응답이 너무 큽니다".into()));
                state.host.error(&e).await
            }
            types::Response::Redirect(to) if inside(&to) => Redirect::to(&to).into_response(),
            types::Response::Redirect(to) => {
                let e = self.fail(Fault::Host(format!("사이트 밖으로 보내려 했습니다: {to}")));
                state.host.error(&e).await
            }
            types::Response::NotFound => state.host.error(&Error::new(Code::NotFound)).await,
        }
    }
}

struct Request {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
}

/// 사이트 안 주소인가: `/` 로 시작하고 `//`·`/\` 가 아니다 (다른 사이트로 가는 길을 막는다).
fn inside(to: &str) -> bool {
    to.starts_with('/')
        && !to.starts_with("//")
        && !to.starts_with("/\\")
        && !to.chars().any(|c| c.is_control())
}

/// 모듈에 넘기는 사람. 토큰 id 같은 내부 값은 주지 않는다.
fn actor_json(a: &Actor) -> Value {
    let (kind, _) = a.audit_identity();
    let perms: Vec<&str> = Permission::ALL
        .into_iter()
        .filter(|p| a.has(*p))
        .map(Permission::as_str)
        .collect();
    json!({ "type": kind, "member_id": a.member_id(), "perms": perms })
}

/// `templates/` 아래 `.html` 을 `mod/<id>/<경로>` 이름으로 읽는다.
fn read_templates(dir: &Path, id: &str) -> Result<Vec<(&'static str, &'static str)>> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| bad(&d, e.to_string()))?;
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "html") {
                continue;
            }
            let text = std::fs::read_to_string(&path).map_err(|e| bad(&path, e.to_string()))?;
            if unsafe_template(&text) {
                return Err(bad(
                    &path,
                    "템플릿에 |safe 나 autoescape false 를 쓸 수 없습니다",
                ));
            }
            total += text.len();
            if total > MAX_TEMPLATES {
                return Err(bad(dir, "템플릿이 모두 합쳐 1MB 를 넘습니다"));
            }
            let rel = path
                .strip_prefix(dir)
                .map_err(|e| bad(&path, e.to_string()))?
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/");
            out.push((leak(format!("mod/{id}/{rel}")), leak(text)));
        }
    }
    out.sort();
    Ok(out)
}

fn event_of(ev: &AuditEvent) -> types::Event {
    types::Event {
        id: ev.id.clone(),
        seq: ev.seq,
        action: ev.action.clone(),
        actor_type: ev.actor_type.clone(),
        actor_id: ev.actor_id.clone(),
        target_type: ev.target_type.clone(),
        target_id: ev.target_id.clone(),
        at: ev.at.clone(),
        diff: ev.diff.to_string(),
    }
}

#[async_trait]
impl Module for ExternalModule {
    fn manifest(&self) -> &'static Manifest {
        self.inner.manifest
    }

    /// 외부 모듈은 표를 만들지 않는다. 제거할 때 키-값 행을 지우도록 down 만 쓴다.
    fn migrations(&self) -> Vec<ModuleMigration> {
        vec![ModuleMigration {
            version: 1,
            name: "ext_module_kv",
            up: "SELECT 1;",
            down: self.inner.cleanup,
        }]
    }

    fn routes(&self) -> Option<Router<ModuleState>> {
        Some(
            Router::new()
                .route("/", any(route))
                .route("/{*rest}", any(route))
                .layer(Extension(self.clone())),
        )
    }

    fn templates(&self) -> Vec<(&'static str, &'static str)> {
        self.inner.templates.clone()
    }

    fn tools(&self) -> Vec<ToolDef> {
        self.inner.tools.clone()
    }

    async fn call_tool(&self, ctx: &Ctx, actor: &Actor, tool: &str, input: Value) -> Result<Value> {
        if !self.inner.tools.iter().any(|t| t.name == tool) {
            return Err(tool_not_found(self.id(), tool));
        }
        let input = input.to_string();
        let actor = actor_json(actor).to_string();
        let out = self
            .run(ctx, Budget::Tool, |mut c| async move {
                c.guest
                    .call_call_tool(&mut c.store, tool, &input, &actor)
                    .await
            })
            .await?;
        self.json_of(&out)
    }

    async fn on_event(&self, ctx: &Ctx, ev: &AuditEvent) -> Result<()> {
        let ev = event_of(ev);
        self.run(ctx, Budget::Event, |mut c| async move {
            c.guest.call_on_event(&mut c.store, &ev).await
        })
        .await
    }

    async fn purge(&self, ctx: &Ctx) -> Result<u64> {
        self.run(ctx, Budget::Tool, |mut c| async move {
            c.guest.call_purge(&mut c.store).await
        })
        .await
    }

    fn settings_fields(&self) -> Vec<FieldDef> {
        self.inner.settings_fields.clone()
    }

    fn default_settings(&self) -> Map<String, Value> {
        self.inner.defaults.clone()
    }

    fn widget_types(&self) -> Vec<WidgetType> {
        self.inner.widgets.clone()
    }

    async fn widget_view(&self, ctx: &Ctx, kind: &str, config: &Value) -> Result<Value> {
        let config = config.to_string();
        let out = self
            .run(ctx, Budget::Screen, |mut c| async move {
                c.guest.call_widget_view(&mut c.store, kind, &config).await
            })
            .await?;
        self.json_of(&out)
    }

    async fn slot(
        &self,
        ctx: &Ctx,
        slot: &str,
        view: &SlotView<'_>,
    ) -> Result<Option<SlotContent>> {
        if !self.inner.slots.iter().any(|s| s == slot) {
            return Ok(None);
        }
        let view = json!({
            "viewer": actor_json(view.viewer),
            "post_id": view.post_id,
            "comment_ids": view.comment_ids,
            "post_ids": view.post_ids,
            "member_ids": view.member_ids,
        })
        .to_string();
        let out = self
            .run(ctx, Budget::Screen, |mut c| async move {
                c.guest.call_slot(&mut c.store, slot, &view).await
            })
            .await?;
        let Some(content) = out else {
            return Ok(None);
        };
        let template = self.template(&content.template).ok_or_else(|| {
            self.fail(Fault::Host(format!(
                "패키지에 없는 템플릿: {}",
                content.template
            )))
        })?;
        Ok(Some(SlotContent {
            template,
            data: self.json_of(&content.data)?,
        }))
    }
}

async fn route(
    AxState(state): AxState<ModuleState>,
    Extension(module): Extension<ExternalModule>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    module
        .handle(
            &state,
            Request {
                method,
                uri,
                headers,
                body,
            },
        )
        .await
}
