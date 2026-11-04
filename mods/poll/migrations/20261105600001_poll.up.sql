-- 투표 모듈 (GAP M14, ADR-0070). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_poll (
    id         TEXT PRIMARY KEY,
    post_id    TEXT NOT NULL UNIQUE,
    question   TEXT NOT NULL,
    multiple   INTEGER NOT NULL DEFAULT 0,
    closes_at  TEXT,
    closed_at  TEXT,
    created_by TEXT,
    at         TEXT NOT NULL
) STRICT;

CREATE TABLE mod_poll_option (
    id      TEXT PRIMARY KEY,
    poll_id TEXT NOT NULL REFERENCES mod_poll (id) ON DELETE CASCADE,
    label   TEXT NOT NULL,
    ord     INTEGER NOT NULL
) STRICT;
CREATE INDEX mod_poll_option_poll ON mod_poll_option (poll_id, ord);

CREATE TABLE mod_poll_vote (
    poll_id   TEXT NOT NULL REFERENCES mod_poll (id) ON DELETE CASCADE,
    option_id TEXT NOT NULL REFERENCES mod_poll_option (id) ON DELETE CASCADE,
    member_id TEXT NOT NULL,
    at        TEXT NOT NULL,
    PRIMARY KEY (poll_id, option_id, member_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX mod_poll_vote_member ON mod_poll_vote (member_id);
