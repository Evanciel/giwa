-- 개인정보 보유기간·휴면 회원 (ADR-0073, GAP M17)
ALTER TABLE members ADD COLUMN last_login_at TEXT;
ALTER TABLE members ADD COLUMN dormant_notice_at TEXT;
UPDATE members SET last_login_at = (
    SELECT max(a.at) FROM audit_log a
    WHERE a.action = 'auth.login' AND a.target_type = 'member' AND a.target_id = members.id
);

-- 휴면 회원의 메일 주소를 따로 둔다. members.email 은 dormant+<id>@invalid 가 된다
CREATE TABLE member_dormant (
    member_id  TEXT PRIMARY KEY NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    email      TEXT NOT NULL COLLATE NOCASE UNIQUE,
    dormant_at TEXT NOT NULL
) STRICT;
