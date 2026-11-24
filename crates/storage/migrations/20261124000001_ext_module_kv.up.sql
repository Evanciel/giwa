-- 외부 모듈(WASM)의 키-값 저장소 (Phase 5, ADR-0177). 외부 모듈에는 SQL 을 주지 않고 이 표만 준다.
-- 키는 200바이트, 값은 64KB, 모듈마다 합쳐 50MB 까지 (한도는 giwa-wasm 이 지킨다).
-- 모듈을 제거하면 그 모듈의 행만 지운다.
CREATE TABLE ext_module_kv (
    module TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (module, key)
);
