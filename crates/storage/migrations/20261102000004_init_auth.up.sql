-- 사람 로그인 (ADR-0004, ADR-0009). 쿠키·링크 원문은 저장하지 않고 SHA-256 해시만 둔다.

CREATE TABLE sessions (
    id           TEXT PRIMARY KEY NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    member_id    TEXT NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    csrf_token   TEXT NOT NULL,              -- 세션에 묶인 CSRF 값 (폼 hidden 필드와 상수 시간 비교)
    created_at   TEXT NOT NULL,
    expires_at   TEXT NOT NULL
) STRICT;
CREATE INDEX sessions_member ON sessions (member_id);
CREATE INDEX sessions_expires ON sessions (expires_at);

CREATE TABLE login_tickets (
    id          TEXT PRIMARY KEY NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    member_id   TEXT NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    used_at     TEXT
) STRICT;
CREATE INDEX login_tickets_member ON login_tickets (member_id);
CREATE INDEX login_tickets_expires ON login_tickets (expires_at);
