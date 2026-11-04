-- 방문자 통계 (GAP S3, ADR-0061). 날짜별 집계만 둔다. IP·해시·전체 Referer 는 남기지 않는다.
CREATE TABLE stat_days (
    day       TEXT PRIMARY KEY NOT NULL,
    views     INTEGER NOT NULL DEFAULT 0,
    visitors  INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE stat_pages (
    day       TEXT NOT NULL,
    path      TEXT NOT NULL,
    views     INTEGER NOT NULL DEFAULT 0,
    visitors  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, path)
) STRICT;
CREATE TABLE stat_referrers (
    day       TEXT NOT NULL,
    host      TEXT NOT NULL,
    visits    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, host)
) STRICT;
