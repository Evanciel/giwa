-- 댓글·대댓글·승인 대기, 비회원 댓글, 자체 캡차 (GAP M5·M6·M10, ADR-0052).
-- 댓글은 posts 와 따로 둔다: 목록·검색 색인·피드·이전/다음 쿼리가 댓글을 몰라도 된다.

CREATE TABLE comments (
    id            TEXT PRIMARY KEY NOT NULL,
    post_id       TEXT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    -- 대댓글이면 뿌리 댓글. 한 단계만 (대댓글에 답해도 같은 뿌리).
    parent_id     TEXT REFERENCES comments (id) ON DELETE CASCADE,
    -- 누구에게 답했는지 (뿌리 또는 같은 뿌리 아래 대댓글). 「@이름」 표시용
    reply_to_id   TEXT,
    status        TEXT NOT NULL CHECK (status IN ('published', 'pending', 'hidden', 'spam', 'deleted')),
    body          TEXT NOT NULL,
    author_id     TEXT REFERENCES members (id) ON DELETE SET NULL,
    guest_name    TEXT,
    guest_pw_hash TEXT,
    actor_type    TEXT NOT NULL CHECK (actor_type IN ('human', 'guest', 'local')),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    CHECK ((actor_type = 'guest') = (guest_name IS NOT NULL AND guest_pw_hash IS NOT NULL))
) STRICT;
CREATE INDEX comments_post ON comments (post_id, status, created_at);
CREATE INDEX comments_status ON comments (status, created_at);
CREATE INDEX comments_parent ON comments (parent_id);
CREATE INDEX comments_author ON comments (author_id, created_at);

-- 1회용 캡차 도전. 서버가 만든 값의 SHA-256 만 둔다. 꺼낼 때 지운다.
CREATE TABLE challenges (
    id_hash    TEXT PRIMARY KEY NOT NULL,
    answer     TEXT NOT NULL,
    issued_at  TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT;
CREATE INDEX challenges_expires ON challenges (expires_at);
