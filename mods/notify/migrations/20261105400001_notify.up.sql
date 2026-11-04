-- 알림 모듈 (GAP M11, ADR-0069). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_notify_item (
    id         TEXT PRIMARY KEY,
    member_id  TEXT NOT NULL,
    kind       TEXT NOT NULL,
    -- comment:<id>, post:<id>. 같은 사람에게 같은 원천은 한 건뿐이다
    source     TEXT NOT NULL,
    post_id    TEXT NOT NULL,
    comment_id TEXT,
    at         TEXT NOT NULL,
    read_at    TEXT,
    UNIQUE (member_id, source)
) STRICT;
CREATE INDEX mod_notify_item_recent ON mod_notify_item (member_id, at DESC);
CREATE INDEX mod_notify_item_source ON mod_notify_item (source);
CREATE INDEX mod_notify_item_post ON mod_notify_item (post_id);

-- 메일 받기를 바꾼 회원만 줄이 있다. 없으면 받는다.
CREATE TABLE mod_notify_pref (
    member_id TEXT PRIMARY KEY,
    mail      INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
