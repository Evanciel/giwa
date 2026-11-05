-- 회원 서명 (GAP Q17, ADR-0102): 글 아래 붙는 짧은 글. 자기소개(member_profiles)와 같은 꼴.
-- 비우면 줄을 지운다. 탈퇴하면 서비스가 지우고, 회원이 지워지면 함께 지워진다.
CREATE TABLE member_signatures (
    member_id  TEXT PRIMARY KEY NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    signature  TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
