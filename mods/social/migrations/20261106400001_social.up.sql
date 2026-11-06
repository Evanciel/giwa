-- 소셜 로그인 모듈 (GAP M15, ADR-0114). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
-- 공급자 설정. google·kakao·naver 는 주소가 정해져 있고, oidc 는 운영자가 주소를 넣는다.
CREATE TABLE mod_social_provider (
    id            TEXT NOT NULL PRIMARY KEY,
    client_id     TEXT NOT NULL,
    client_secret TEXT NOT NULL DEFAULT '',
    label         TEXT NOT NULL DEFAULT '',
    auth_url      TEXT NOT NULL DEFAULT '',
    token_url     TEXT NOT NULL DEFAULT '',
    userinfo_url  TEXT NOT NULL DEFAULT '',
    enabled       INTEGER NOT NULL DEFAULT 1,
    updated_at    TEXT NOT NULL
) STRICT, WITHOUT ROWID;
-- (공급자, 공급자 안 사용자 ID) → 회원. 이메일은 연결할 때 공급자가 알려 준 값(표시용).
CREATE TABLE mod_social_identity (
    provider      TEXT NOT NULL,
    subject       TEXT NOT NULL,
    member_id     TEXT NOT NULL,
    email         TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    last_login_at TEXT,
    PRIMARY KEY (provider, subject)
) STRICT, WITHOUT ROWID;
CREATE INDEX mod_social_identity_member ON mod_social_identity (member_id);
-- 진행 중인 로그인(state)·가입(join). 원문은 쿠키에만, 여기에는 SHA-256 만 (ADR-0009).
CREATE TABLE mod_social_flow (
    hash       TEXT NOT NULL PRIMARY KEY,
    kind       TEXT NOT NULL,
    provider   TEXT NOT NULL,
    data       TEXT NOT NULL,
    expires_at TEXT NOT NULL
) STRICT, WITHOUT ROWID;
