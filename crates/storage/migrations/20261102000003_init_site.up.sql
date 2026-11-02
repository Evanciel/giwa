-- init_site: 페이지와 메뉴 (docs/PHASE1_PLAN.md §5, U9)
-- pages.layout, menus.location 은 늘어날 값이라 CHECK 없이 core 가 검증한다 (ADR-0016).

CREATE TABLE pages (
    id           TEXT PRIMARY KEY NOT NULL,
    slug         TEXT NOT NULL UNIQUE,
    title        TEXT NOT NULL,
    body_md      TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('draft', 'published', 'hidden', 'deleted')),
    layout       TEXT NOT NULL,
    author_id    TEXT REFERENCES members (id),
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('human', 'agent', 'local')),
    sort_order   INTEGER NOT NULL DEFAULT 0,
    published_at TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

-- 메뉴는 위치(header, footer)마다 트리 전체를 한 번에 바꾼다. link 는 JSON {kind, ...}.
CREATE TABLE menus (
    id         TEXT PRIMARY KEY NOT NULL,
    location   TEXT NOT NULL,
    parent_id  TEXT REFERENCES menus (id) ON DELETE CASCADE,
    label      TEXT NOT NULL,
    link       TEXT NOT NULL,
    sort_order INTEGER NOT NULL,
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX menus_location ON menus (location, sort_order);
