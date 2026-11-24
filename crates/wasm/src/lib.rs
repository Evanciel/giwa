//! 외부 모듈(WASM 컴포넌트) 런타임 (Phase 5, ADR-0176·ADR-0177).
//!
//! `<데이터>/modules/<id>/` 의 `module.toml`·`module.wasm`·`templates/` 를 읽어 내장 모듈과 같은
//! [`giwa_modules::Module`] 로 내놓는다. 웹·REST·CLI·MCP 는 외부 모듈인지 모른다(원칙 1).
//! 호스트 API 는 `wit/giwa-module.wit`.
//!
//! 1단계: 시작할 때 폴더에서 읽는다. 패키지·서명·설치는 2단계, 글·HTTP 등 호스트 API 와 권한은 3단계.

mod kv;
mod manifest;
mod module;
mod runtime;

mod bindings {
    wasmtime::component::bindgen!({
        world: "module",
        path: "wit",
        imports: { default: async },
        exports: { default: async },
    });
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use giwa_storage::Db;

pub use manifest::API;
pub use module::ExternalModule;

/// 한 프로세스에서 같은 폴더를 여러 번 읽어도(CLI 가 레지스트리를 몇 번 만든다) 한 번만 읽고
/// 컴파일도 한 번만 한다. 매니페스트의 글자는 `'static` 으로 새어 나가므로 다시 읽지 않는 편이 낫다.
static LOADED: Mutex<Option<HashMap<PathBuf, Vec<Arc<ExternalModule>>>>> = Mutex::new(None);

/// `dir`(보통 `<데이터>/modules`) 아래의 외부 모듈. 폴더가 없으면 빈 목록. 잘못된 모듈은 경고하고
/// 건너뛴다(사이트가 모듈 하나 때문에 뜨지 않으면 안 된다). `taken` 은 이미 있는 모듈 id(내장)다.
pub fn load(dir: &Path, db: &Db, taken: &[&str]) -> Vec<Arc<ExternalModule>> {
    let Ok(mut guard) = LOADED.lock() else {
        return Vec::new();
    };
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(found) = cache.get(dir) {
        return found.clone();
    }
    let mut out: Vec<Arc<ExternalModule>> = Vec::new();
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    for path in entries {
        if !path.join("module.toml").is_file() {
            continue;
        }
        match ExternalModule::open(&path, db.clone()) {
            Ok(m) => {
                let id = m.id();
                if taken.contains(&id) || out.iter().any(|o| o.id() == id) {
                    tracing::warn!(module = id, path = %path.display(), "내장 모듈이나 다른 외부 모듈과 id 가 겹쳐 건너뜁니다");
                    continue;
                }
                out.push(Arc::new(m));
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "외부 모듈을 읽지 못해 건너뜁니다");
            }
        }
    }
    cache.insert(dir.to_path_buf(), out.clone());
    out
}
