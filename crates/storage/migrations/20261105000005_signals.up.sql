-- 운영 신호 (ADR-0074, GAP M19). 알린 신호를 기억해 같은 신호를 두 번 보내지 않는다
CREATE TABLE signal_sent (
    key TEXT PRIMARY KEY NOT NULL,
    at  TEXT NOT NULL
) STRICT, WITHOUT ROWID;
-- 지난 1시간 댓글을 글마다 셀 때 표 전체를 훑지 않게
CREATE INDEX comments_created ON comments (created_at);
