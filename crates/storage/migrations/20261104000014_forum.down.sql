DROP INDEX posts_board_activity;
ALTER TABLE posts DROP COLUMN is_locked;
ALTER TABLE posts DROP COLUMN accepted_comment_id;
ALTER TABLE posts DROP COLUMN last_activity_at;
ALTER TABLE boards DROP COLUMN list_order;
