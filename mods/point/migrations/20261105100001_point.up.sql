-- 포인트 모듈 (GAP M13, ADR-0067). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_point_ledger (
    id         TEXT PRIMARY KEY NOT NULL,
    member_id  TEXT NOT NULL,
    amount     INTEGER NOT NULL,
    reason     TEXT NOT NULL,
    -- 같은 일로 두 번 적립하지 않게 하는 열쇠 (`post:<id>`, `login:<회원>:<날>` …)
    source     TEXT NOT NULL UNIQUE,
    at         TEXT NOT NULL
) STRICT;
CREATE INDEX mod_point_ledger_member ON mod_point_ledger (member_id, at DESC);

CREATE TABLE mod_point_balance (
    member_id  TEXT PRIMARY KEY NOT NULL,
    balance    INTEGER NOT NULL
) STRICT;
CREATE INDEX mod_point_balance_rank ON mod_point_balance (balance DESC, member_id);
