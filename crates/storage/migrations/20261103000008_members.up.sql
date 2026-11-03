-- 회원 가입·메일 인증·비밀번호 찾기·탈퇴 (GAP M1·M2·M3·S5, ADR-0051).
-- members 를 다시 만들면 ON DELETE CASCADE 로 세션·역할이 지워질 수 있어 열만 더한다.

ALTER TABLE members ADD COLUMN email_verified_at TEXT;
ALTER TABLE members ADD COLUMN withdrawn_at TEXT;
UPDATE members SET email_verified_at = created_at;
CREATE UNIQUE INDEX members_display_name ON members (display_name COLLATE NOCASE);

-- login: 로그인 링크(10분) · verify: 메일 인증(24시간) · reset: 비밀번호 재설정(1시간)
ALTER TABLE login_tickets ADD COLUMN purpose TEXT NOT NULL DEFAULT 'login'
    CHECK (purpose IN ('login', 'verify', 'reset'));

-- 보낼 편지함. 같은 트랜잭션에서 쌓고 배달은 서버가 따로 한다. 같은 idem_key 는 한 번만 쌓인다.
CREATE TABLE mail_outbox (
    id         TEXT PRIMARY KEY NOT NULL,
    idem_key   TEXT NOT NULL UNIQUE,
    to_addr    TEXT NOT NULL,
    subject    TEXT NOT NULL,
    body       TEXT NOT NULL,
    status     TEXT NOT NULL CHECK (status IN ('queued', 'sent', 'failed')),
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    sent_at    TEXT
) STRICT;
CREATE INDEX mail_outbox_status ON mail_outbox (status, created_at);
