-- 공개 프로필의 자기소개 (GAP Q2, ADR-0087). 회원 표를 고치지 않고 따로 둔다.
-- 비우면 줄을 지운다. 탈퇴하면 서비스가 지우고, 회원이 지워지면 함께 지워진다.
CREATE TABLE member_profiles (
    member_id  TEXT PRIMARY KEY NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    bio        TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
