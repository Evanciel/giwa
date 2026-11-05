-- 나가는 웹훅 모듈 (GAP S13, ADR-0085). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
-- 서명 키(`secret`)는 서명을 만들 때 원래 값이 있어야 하므로 해시가 아니라 그대로 둔다(ADR-0085).
CREATE TABLE mod_webhook_endpoint (
    id          TEXT NOT NULL PRIMARY KEY,
    url         TEXT NOT NULL,
    events      TEXT NOT NULL,
    secret      TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL
) STRICT;

-- 받는 곳·이벤트마다 한 줄. 이벤트 버스가 다시 시도할 때 이미 받은 곳에는 다시 보내지 않는다.
CREATE TABLE mod_webhook_delivery (
    endpoint_id TEXT NOT NULL REFERENCES mod_webhook_endpoint (id) ON DELETE CASCADE,
    event_id    TEXT NOT NULL,
    action      TEXT NOT NULL,
    ok          INTEGER NOT NULL,
    attempts    INTEGER NOT NULL,
    http_status INTEGER,
    last_error  TEXT,
    at          TEXT NOT NULL,
    PRIMARY KEY (endpoint_id, event_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX mod_webhook_delivery_recent ON mod_webhook_delivery (at DESC);
