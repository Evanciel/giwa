-- 포럼형 게시판 (ADR-0058). list_order 는 목록 순서(빈 값이면 종류의 기본값),
-- last_activity_at 은 마지막 공개 댓글·답변 시각(없으면 발행 시각), accepted_comment_id 는 채택한 댓글,
-- is_locked 는 관리자 말고는 댓글·답변·고치기·지우기를 막는다.
ALTER TABLE boards ADD COLUMN list_order TEXT NOT NULL DEFAULT '';
ALTER TABLE posts ADD COLUMN last_activity_at TEXT;
ALTER TABLE posts ADD COLUMN accepted_comment_id TEXT;
ALTER TABLE posts ADD COLUMN is_locked INTEGER NOT NULL DEFAULT 0;
CREATE INDEX posts_board_activity ON posts (board_id, last_activity_at);
