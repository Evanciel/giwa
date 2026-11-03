-- 위젯 배치 (GAP S1, ADR-0059). 종류별 설정은 config JSON, 검사는 core 가 한다.
CREATE TABLE widgets (
    id          TEXT PRIMARY KEY NOT NULL,
    kind        TEXT NOT NULL,
    region      TEXT NOT NULL,
    scope       TEXT NOT NULL DEFAULT 'all',
    title       TEXT NOT NULL DEFAULT '',
    config      TEXT NOT NULL DEFAULT '{}',
    sort_order  INTEGER NOT NULL DEFAULT 0,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;
CREATE INDEX widgets_region ON widgets (region, sort_order);
