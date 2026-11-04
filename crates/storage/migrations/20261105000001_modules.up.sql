-- 모듈 시스템 (SPEC §7, ADR-0066). 줄이 있으면 설치된 모듈이다.
CREATE TABLE modules (
    id           TEXT PRIMARY KEY NOT NULL,
    enabled      INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    version      TEXT NOT NULL,
    settings     TEXT NOT NULL DEFAULT '{}',
    -- 이 모듈이 여기까지 받은 audit_log 의 rowid
    cursor       INTEGER NOT NULL DEFAULT 0,
    installed_at TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

-- 모듈 마이그레이션 기록. 코어의 _sqlx_migrations 와 섞지 않아 켜고 지우기가 코어 버전 검사에 안 걸린다.
CREATE TABLE module_migrations (
    module     TEXT NOT NULL,
    version    INTEGER NOT NULL,
    name       TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    PRIMARY KEY (module, version)
) STRICT;

-- 처리하지 못한 이벤트. 재시도 대기와 실패 보관함을 겸한다.
CREATE TABLE module_event_failures (
    module     TEXT NOT NULL,
    event_id   TEXT NOT NULL,
    action     TEXT NOT NULL,
    attempts   INTEGER NOT NULL,
    last_error TEXT NOT NULL,
    next_at    TEXT NOT NULL,
    gave_up    INTEGER NOT NULL DEFAULT 0 CHECK (gave_up IN (0, 1)),
    first_at   TEXT NOT NULL,
    PRIMARY KEY (module, event_id)
) STRICT;
CREATE INDEX module_event_failures_due ON module_event_failures (module, gave_up, next_at);
