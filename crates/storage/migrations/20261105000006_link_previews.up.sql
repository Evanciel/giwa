-- 링크 카드 (ADR-0084, GAP E12). 주소마다 한 줄, 여러 글이 함께 쓴다
CREATE TABLE link_previews (
    url         TEXT PRIMARY KEY NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'ok', 'failed')),
    title       TEXT,
    description TEXT,
    site_name   TEXT,
    image_url   TEXT,
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    fetched_at  TEXT,
    created_at  TEXT NOT NULL
) STRICT, WITHOUT ROWID;
CREATE INDEX link_previews_pending ON link_previews (status, created_at);
