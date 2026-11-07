-- 페이지 계층과 옮긴 주소 (ADR-0021, ADR-0128).
ALTER TABLE pages ADD COLUMN parent_id TEXT;
CREATE INDEX idx_pages_parent ON pages(parent_id);

CREATE TABLE redirects (
    from_path  TEXT PRIMARY KEY,
    to_path    TEXT NOT NULL,
    created_at TEXT NOT NULL
) WITHOUT ROWID;
