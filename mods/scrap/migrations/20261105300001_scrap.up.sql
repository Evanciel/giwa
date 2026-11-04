-- 스크랩 모듈 (GAP M8, ADR-0068). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_scrap_item (
    member_id TEXT NOT NULL,
    post_id   TEXT NOT NULL,
    at        TEXT NOT NULL,
    PRIMARY KEY (member_id, post_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX mod_scrap_item_recent ON mod_scrap_item (member_id, at DESC);
