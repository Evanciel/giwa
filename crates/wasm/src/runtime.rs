//! wasmtime 엔진과 한 번 부를 때의 상자 (ADR-0176·ADR-0177).
//!
//! - 엔진은 프로세스에 하나. 외부 모듈을 처음 부를 때 만든다(없으면 유휴 비용이 0).
//! - 부를 때마다 새 인스턴스. 시간은 epoch(100ms 눈금)로 끊고, 메모리는 64MB, 동시에 2개까지.
//! - 컴파일한 결과는 `<모듈 폴더>/.cache/<해시>.cwasm` 에 두고 다음 시작 때 읽는다.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use giwa_storage::Db;
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, SemaphorePermit};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Strategy};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::giwa::module::{host, types};
use crate::bindings::{Module as Guest, ModulePre};
use crate::kv;

/// epoch 눈금
const TICK: Duration = Duration::from_millis(100);
const MEMORY: usize = 64 << 20;
/// N100 에서 함께 돌 외부 모듈 호출 수
const CONCURRENCY: usize = 2;
/// 한 번 부를 때 모듈이 남길 수 있는 로그
const LOG_BUDGET: usize = 16 * 1024;
/// 손님이 돌려주는 글(JSON·글자·화면 값)의 한도
pub const MAX_OUTPUT: usize = 1_000_000;

/// 부르는 쪽마다 시간 한도.
#[derive(Debug, Clone, Copy)]
pub enum Budget {
    Event,
    Screen,
    Tool,
}

impl Budget {
    fn limit(self) -> Duration {
        match self {
            Self::Event => Duration::from_secs(2),
            Self::Screen => Duration::from_secs(1),
            Self::Tool => Duration::from_secs(5),
        }
    }
}

pub struct Runtime {
    engine: Engine,
    linker: Linker<State>,
    slots: Semaphore,
}

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

/// 실행 실패. 손님이 낸 실패(`module-error`)와 상자 밖으로 나온 실패(트랩·시간 초과·컴파일)를 나눈다.
#[derive(Debug)]
pub enum Fault {
    Guest(types::ModuleError),
    Host(String),
}

impl From<wasmtime::Error> for Fault {
    fn from(e: wasmtime::Error) -> Self {
        Self::Host(short(&e))
    }
}

pub fn short(e: &dyn std::fmt::Display) -> String {
    format!("{e:#}").chars().take(300).collect()
}

pub fn runtime() -> Result<&'static Runtime, Fault> {
    RUNTIME
        .get_or_init(|| Runtime::new().map_err(|e| short(&e)))
        .as_ref()
        .map_err(|e| Fault::Host(e.clone()))
}

impl Runtime {
    fn new() -> wasmtime::Result<Self> {
        let mut c = Config::new();
        c.strategy(Strategy::Cranelift);
        c.epoch_interruption(true);
        let engine = Engine::new(&c)?;
        let mut linker: Linker<State> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        Guest::add_to_linker::<State, HasSelf<State>>(&mut linker, |s| s)?;
        let ticker = engine.clone();
        std::thread::Builder::new()
            .name("giwa-wasm-epoch".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(TICK);
                    ticker.increment_epoch();
                }
            })?;
        Ok(Self {
            engine,
            linker,
            slots: Semaphore::new(CONCURRENCY),
        })
    }

    /// 컴포넌트를 읽고 링크까지 해 둔다. 무거우므로 블로킹 스레드에서 부른다.
    pub fn prepare(&self, wasm: &Path) -> Result<ModulePre<State>, String> {
        let bytes = std::fs::read(wasm).map_err(|e| format!("{}: {e}", wasm.display()))?;
        let cache = self.cache_path(wasm, &bytes);
        let component = match self.load_cached(&cache) {
            Some(c) => c,
            None => {
                let c = Component::new(&self.engine, &bytes).map_err(|e| short(&e))?;
                self.store_cache(&cache, &c);
                c
            }
        };
        let pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(|e| short(&e))?;
        ModulePre::new(pre).map_err(|e| short(&e))
    }

    /// wasm 바이트와 엔진 설정이 같을 때만 같은 이름이 된다.
    fn cache_path(&self, wasm: &Path, bytes: &[u8]) -> PathBuf {
        let mut h = DefaultHasher::new();
        self.engine.precompile_compatibility_hash().hash(&mut h);
        let mut sha = Sha256::new();
        sha.update(bytes);
        sha.update(h.finish().to_le_bytes());
        let name = hex::encode(&sha.finalize()[..16]);
        let dir = wasm.parent().unwrap_or(Path::new(".")).join(".cache");
        dir.join(format!("{name}.cwasm"))
    }

    #[allow(unsafe_code)]
    fn load_cached(&self, path: &Path) -> Option<Component> {
        if !path.is_file() {
            return None;
        }
        // SAFETY: 이 파일은 기와가 `store_cache` 에서 같은 엔진 설정으로 직접 만든 것이다. 이름이
        // wasm 바이트와 엔진 설정의 해시라 다른 파일과 섞이지 않는다. 남이 준 .cwasm 은 받지 않는다
        // (계획 「하지 않는 것」). 데이터 폴더에 쓸 수 있는 사람은 이미 DB 도 바꿀 수 있다.
        match unsafe { Component::deserialize_file(&self.engine, path) } {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %short(&e), "모듈 캐시를 버리고 다시 컴파일합니다");
                let _ = std::fs::remove_file(path);
                None
            }
        }
    }

    fn store_cache(&self, path: &Path, component: &Component) {
        let done = (|| -> std::io::Result<()> {
            let bytes = component
                .serialize()
                .map_err(|e| std::io::Error::other(short(&e)))?;
            let dir = path.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(dir)?;
            // 예전 판의 캐시는 지운다.
            for entry in std::fs::read_dir(dir)?.flatten() {
                if entry.path().extension().is_some_and(|e| e == "cwasm") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = done {
            tracing::warn!(path = %path.display(), error = %e, "모듈 캐시를 쓰지 못했습니다");
        }
    }

    /// 새 상자를 만들고 인스턴스를 띄운다. 돌려준 허가증을 쥐고 있는 동안 자리 하나를 쓴다.
    pub async fn instance(
        &'static self,
        pre: &ModulePre<State>,
        state: State,
        budget: Budget,
    ) -> Result<Call, Fault> {
        let permit = self
            .slots
            .acquire()
            .await
            .map_err(|e| Fault::Host(e.to_string()))?;
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        let ticks = budget.limit().as_millis() / TICK.as_millis();
        store.set_epoch_deadline(u64::try_from(ticks).unwrap_or(10));
        store.epoch_deadline_trap();
        let guest = pre.instantiate_async(&mut store).await?;
        Ok(Call {
            store,
            guest,
            budget,
            _permit: permit,
        })
    }
}

/// 띄운 인스턴스 하나. 한 번 부르고 버린다.
pub struct Call {
    pub store: Store<State>,
    pub guest: Guest,
    pub budget: Budget,
    _permit: SemaphorePermit<'static>,
}

impl Call {
    /// 호스트 함수(DB 등)에서 멈추면 epoch 가 끊지 못하므로 바깥에서도 시간을 잰다.
    pub fn deadline(&self) -> Duration {
        self.budget.limit() + Duration::from_secs(1)
    }
}

/// 상자 안 상태. 모듈이 부르는 호스트 함수가 여기서 일한다.
pub struct State {
    limits: StoreLimits,
    wasi: WasiCtx,
    table: ResourceTable,
    module: &'static str,
    db: Db,
    settings: String,
    logged: usize,
}

impl State {
    pub fn new(module: &'static str, db: Db, settings: String) -> Self {
        Self {
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY)
                .instances(16)
                .build(),
            // 빈 상자: 폴더·소켓·환경 변수·인자 없음. 표준 출력·오류는 버린다.
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
            module,
            db,
            settings,
            logged: 0,
        }
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

fn guest_error((code, message): kv::KvError) -> types::ModuleError {
    types::ModuleError {
        code: code.to_owned(),
        message,
    }
}

impl types::Host for State {}

impl host::Host for State {
    async fn log(&mut self, level: host::Level, message: String) {
        if self.logged >= LOG_BUDGET {
            return;
        }
        let room = LOG_BUDGET - self.logged;
        let text: String = if message.len() > room {
            let mut end = room;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message[..end].to_owned()
        } else {
            message
        };
        self.logged += text.len();
        let module = self.module;
        match level {
            host::Level::Debug => tracing::debug!(module, "{text}"),
            host::Level::Info => tracing::info!(module, "{text}"),
            host::Level::Warn => tracing::warn!(module, "{text}"),
            host::Level::Error => tracing::error!(module, "{text}"),
        }
    }

    async fn now(&mut self) -> String {
        giwa_core::clock::now()
    }

    async fn settings(&mut self) -> String {
        self.settings.clone()
    }

    async fn kv_get(&mut self, key: String) -> Result<Option<Vec<u8>>, types::ModuleError> {
        kv::get(&self.db, self.module, &key)
            .await
            .map_err(guest_error)
    }

    async fn kv_set(&mut self, key: String, value: Vec<u8>) -> Result<(), types::ModuleError> {
        kv::set(&self.db, self.module, &key, &value)
            .await
            .map_err(guest_error)
    }

    async fn kv_delete(&mut self, key: String) -> Result<bool, types::ModuleError> {
        kv::delete(&self.db, self.module, &key)
            .await
            .map_err(guest_error)
    }

    async fn kv_list(
        &mut self,
        prefix: String,
        after: Option<String>,
        limit: u32,
    ) -> Result<Vec<(String, Vec<u8>)>, types::ModuleError> {
        kv::list(&self.db, self.module, &prefix, after.as_deref(), limit)
            .await
            .map_err(guest_error)
    }
}
