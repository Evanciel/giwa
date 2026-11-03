DROP INDEX posts_thread;
DROP INDEX posts_parent;
ALTER TABLE posts DROP COLUMN thread_owner;
ALTER TABLE posts DROP COLUMN thread_path;
ALTER TABLE posts DROP COLUMN thread_id;
ALTER TABLE posts DROP COLUMN parent_id;
