-- 신고 모듈 (GAP M9, ADR-0071). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_report_item (
    id          TEXT PRIMARY KEY,
    target_type TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    reporter_id TEXT NOT NULL,
    reason      TEXT NOT NULL,
    note        TEXT,
    status      TEXT NOT NULL,
    at          TEXT NOT NULL,
    resolved_at TEXT,
    resolved_by TEXT,
    UNIQUE (target_type, target_id, reporter_id)
) STRICT;
CREATE INDEX mod_report_item_queue ON mod_report_item (status, at);
CREATE INDEX mod_report_item_reporter ON mod_report_item (reporter_id);

-- 자동으로 가린 대상. 기각할 때 이것만 되살린다.
CREATE TABLE mod_report_hidden (
    target_type TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    at          TEXT NOT NULL,
    PRIMARY KEY (target_type, target_id)
) STRICT, WITHOUT ROWID;
