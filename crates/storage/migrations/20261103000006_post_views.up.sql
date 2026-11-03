-- 조회수 (GAP C6, ADR-0049). 서버가 모아 두었다가 주기마다 더한다. 올려도 updated_at 은 바꾸지 않는다.
-- 이전·다음 글은 기존 posts_board_status_published (board_id, status, published_at) 인덱스를 쓴다.
ALTER TABLE posts ADD COLUMN view_count INTEGER NOT NULL DEFAULT 0 CHECK (view_count >= 0);
