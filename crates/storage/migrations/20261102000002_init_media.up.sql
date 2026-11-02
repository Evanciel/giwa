-- init_media: SPEC §15 Phase 1 목록에 있어 만든다. Phase 1 에는 읽는 코드가 없다 (ADR-0010).
CREATE TABLE media (
    id         TEXT PRIMARY KEY NOT NULL,
    path       TEXT NOT NULL UNIQUE,
    mime       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    width      INTEGER,
    height     INTEGER,
    alt_text   TEXT,
    variants   TEXT NOT NULL DEFAULT '[]',  -- JSON
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
