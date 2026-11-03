-- 분류·태그·공지·비밀글 (GAP C1~C4, docs/plans/c1-c4-category-tag-notice-secret.md).
-- 분류는 게시판별, 태그는 사이트 전체 하나.
-- posts.category_id 에는 외래키를 걸지 않는다: SQLite 는 외래키 열을 DROP COLUMN 하지 못해
-- down 이 불가능해진다. 분류를 지우면 core 가 같은 트랜잭션에서 글의 분류를 비운다.

CREATE TABLE categories (
    id         TEXT PRIMARY KEY NOT NULL,
    board_id   TEXT NOT NULL REFERENCES boards (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (board_id, slug)
) STRICT;

CREATE TABLE tags (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL,
    slug       TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
) STRICT;

-- position: 글쓴이가 적은 태그 순서.
CREATE TABLE post_tags (
    post_id  TEXT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    tag_id   TEXT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (post_id, tag_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX post_tags_tag ON post_tags (tag_id);

ALTER TABLE posts ADD COLUMN category_id TEXT;
ALTER TABLE posts ADD COLUMN is_notice INTEGER NOT NULL DEFAULT 0 CHECK (is_notice IN (0, 1));
ALTER TABLE posts ADD COLUMN is_secret INTEGER NOT NULL DEFAULT 0 CHECK (is_secret IN (0, 1));
CREATE INDEX posts_category ON posts (category_id) WHERE category_id IS NOT NULL;
CREATE INDEX posts_notice ON posts (board_id, published_at) WHERE is_notice = 1;
