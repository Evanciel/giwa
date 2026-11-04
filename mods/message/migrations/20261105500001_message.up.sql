-- 쪽지 모듈 (GAP M12, ADR-0070). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_message_item (
    id                TEXT PRIMARY KEY,
    sender_id         TEXT NOT NULL,
    recipient_id      TEXT NOT NULL,
    body              TEXT NOT NULL,
    at                TEXT NOT NULL,
    read_at           TEXT,
    sender_deleted    INTEGER NOT NULL DEFAULT 0,
    recipient_deleted INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX mod_message_item_in ON mod_message_item (recipient_id, at DESC);
CREATE INDEX mod_message_item_out ON mod_message_item (sender_id, at DESC);

CREATE TABLE mod_message_block (
    member_id  TEXT NOT NULL,
    blocked_id TEXT NOT NULL,
    at         TEXT NOT NULL,
    PRIMARY KEY (member_id, blocked_id)
) STRICT, WITHOUT ROWID;
