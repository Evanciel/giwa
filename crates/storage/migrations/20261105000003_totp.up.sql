-- 2단계 인증(TOTP, GAP M16, ADR-0072).
-- 비밀은 앱이 같은 값을 계산해야 하므로 해시가 아니라 원문(base32)으로 둔다. 비밀 뺀 백업에서는 지운다.

-- enabled_at 이 없으면 설정 중(코드를 아직 확인하지 않음). last_step 은 쓴 코드의 30초 순번(같은 코드 다시 쓰기 막기).
CREATE TABLE member_totp (
    member_id  TEXT PRIMARY KEY NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    secret     TEXT NOT NULL,
    enabled_at TEXT,
    last_step  INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
) STRICT;

-- 앱을 잃었을 때 쓰는 1회용 복구 코드. SHA-256 해시만 둔다.
CREATE TABLE member_recovery_codes (
    id         TEXT PRIMARY KEY NOT NULL,
    member_id  TEXT NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    code_hash  TEXT NOT NULL UNIQUE,
    used_at    TEXT
) STRICT;
CREATE INDEX member_recovery_codes_member ON member_recovery_codes (member_id);

-- 비밀번호(또는 메일 링크)를 확인한 뒤 코드를 기다리는 로그인. 5분, 틀리면 5번까지.
-- login_tickets.purpose 의 CHECK 를 바꾸려면 표를 다시 만들어야 해서 따로 둔다.
CREATE TABLE totp_challenges (
    id         TEXT PRIMARY KEY NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    member_id  TEXT NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    method     TEXT NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT;
CREATE INDEX totp_challenges_expires ON totp_challenges (expires_at);
