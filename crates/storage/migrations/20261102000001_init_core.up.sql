-- init_core: Phase 1 핵심 테이블 (docs/PHASE1_PLAN.md §5, ADR-0010·0016·0029)
-- 모든 테이블은 STRICT. ID 는 ULID 문자열, 시각은 UTC RFC3339 문자열.
-- CHECK 는 상태 기계 값에만 건다. 늘어날 열거 값(boards.kind, audit_log.action 등)은 core 가 검증한다.

CREATE TABLE site_settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,              -- JSON
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE members (
    id            TEXT PRIMARY KEY NOT NULL,
    email         TEXT NOT NULL COLLATE NOCASE UNIQUE,
    display_name  TEXT NOT NULL,
    password_hash TEXT,                     -- NULL = 아직 비밀번호 없음 (로그인 링크로만 진입, ADR-0004)
    status        TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE TABLE roles (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    rank        INTEGER NOT NULL,           -- 게시판 정책 비교용: guest 0 < member 10 < editor 20 < admin 30 < owner 40
    permissions TEXT NOT NULL,              -- JSON 배열 (SPEC §5.1 액션)
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

CREATE TABLE member_roles (
    member_id TEXT NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    role_id   TEXT NOT NULL REFERENCES roles (id) ON DELETE CASCADE,
    PRIMARY KEY (member_id, role_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE tokens (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,      -- SHA-256 hex (ADR-0009)
    scopes       TEXT NOT NULL,             -- JSON 배열
    board_ids    TEXT,                      -- JSON 배열, NULL = 전체
    expires_at   TEXT,
    last_used_at TEXT,
    revoked_at   TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

CREATE TABLE audit_log (
    id          TEXT PRIMARY KEY NOT NULL,
    actor_type  TEXT NOT NULL CHECK (actor_type IN ('anonymous', 'human', 'agent', 'local', 'system')),
    actor_id    TEXT,
    action      TEXT NOT NULL,
    target_type TEXT NOT NULL,
    target_id   TEXT,
    diff        TEXT NOT NULL,              -- JSON. 비밀 값은 core 가 지운 뒤 들어온다
    at          TEXT NOT NULL
) STRICT;
CREATE INDEX audit_log_target ON audit_log (target_type, target_id);
CREATE INDEX audit_log_at ON audit_log (at);

CREATE TABLE boards (
    id         TEXT PRIMARY KEY NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    policy     TEXT NOT NULL,               -- JSON {list, read, write, moderate}
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

-- status 에는 SPEC §4 의 값을 모두 둔다. pending·spam 은 Phase 4 댓글용이지만
-- CHECK 변경은 테이블 재작성이 필요하므로 지금 넣어 둔다 (ADR-0016). deleted 는 ADR-0029.
CREATE TABLE posts (
    id           TEXT PRIMARY KEY NOT NULL,
    board_id     TEXT NOT NULL REFERENCES boards (id),
    kind         TEXT NOT NULL CHECK (kind IN ('post', 'comment')),
    status       TEXT NOT NULL CHECK (status IN ('draft', 'review', 'published', 'hidden', 'pending', 'spam', 'deleted')),
    title        TEXT NOT NULL,
    slug         TEXT NOT NULL,
    body_md      TEXT NOT NULL,
    excerpt      TEXT,
    author_id    TEXT REFERENCES members (id),
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('human', 'agent', 'local')),
    published_at TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    UNIQUE (board_id, slug)
) STRICT;
CREATE INDEX posts_board_status_published ON posts (board_id, status, published_at);

CREATE TABLE revisions (
    id         TEXT PRIMARY KEY NOT NULL,
    post_id    TEXT REFERENCES posts (id) ON DELETE CASCADE,
    page_id    TEXT,                        -- pages 는 init_site(U9)에서 생긴다
    title      TEXT NOT NULL,
    body_md    TEXT NOT NULL,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('human', 'agent', 'local')),
    actor_id   TEXT,
    created_at TEXT NOT NULL,
    CHECK ((post_id IS NULL) <> (page_id IS NULL))
) STRICT;
CREATE INDEX revisions_post ON revisions (post_id, created_at);
CREATE INDEX revisions_page ON revisions (page_id, created_at);

-- 기본 역할 (SPEC §4, §5.1). ID 가 고정 문자열이다.
INSERT INTO roles (id, name, rank, permissions, created_at, updated_at) VALUES
 ('role_guest',  'guest',  0,  '["site.view","board.list","post.read"]', '2026-11-02T00:00:00Z', '2026-11-02T00:00:00Z'),
 ('role_member', 'member', 10, '["site.view","board.list","post.read","post.write","post.comment","post.upload"]', '2026-11-02T00:00:00Z', '2026-11-02T00:00:00Z'),
 ('role_editor', 'editor', 20, '["site.view","board.list","post.read","post.write","post.comment","post.upload","post.publish","post.moderate"]', '2026-11-02T00:00:00Z', '2026-11-02T00:00:00Z'),
 ('role_admin',  'admin',  30, '["site.view","board.list","post.read","post.write","post.comment","post.upload","post.publish","post.moderate","structure.edit","design.edit","module.manage","member.manage"]', '2026-11-02T00:00:00Z', '2026-11-02T00:00:00Z'),
 ('role_owner',  'owner',  40, '["site.view","board.list","post.read","post.write","post.comment","post.upload","post.publish","post.moderate","structure.edit","design.edit","module.manage","member.manage","site.admin"]', '2026-11-02T00:00:00Z', '2026-11-02T00:00:00Z');
