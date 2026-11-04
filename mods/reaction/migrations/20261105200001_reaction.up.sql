-- 반응 모듈 (GAP M7, ADR-0068). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_reaction_item (
    target_type TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    -- `m:<회원>` 또는 `g:<해시>`. 대상마다 한 사람 하나
    who         TEXT NOT NULL,
    member_id   TEXT,
    kind        TEXT NOT NULL,
    at          TEXT NOT NULL,
    PRIMARY KEY (target_type, target_id, who)
) STRICT, WITHOUT ROWID;
CREATE INDEX mod_reaction_item_member ON mod_reaction_item (member_id);

-- 손님 해시의 소금처럼 모듈만 쓰는 값
CREATE TABLE mod_reaction_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;
