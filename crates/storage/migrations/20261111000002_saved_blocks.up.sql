-- 내 블록 (ADR-0159). 화면 편집기에서 저장해 다른 화면에 다시 넣는 블록. 사이트에 보이지 않는 보관함이다
CREATE TABLE saved_blocks (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    region     TEXT NOT NULL,
    title      TEXT NOT NULL DEFAULT '',
    config     TEXT NOT NULL,
    created_by TEXT,
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX saved_blocks_created ON saved_blocks (created_at);
