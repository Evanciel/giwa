DROP INDEX IF EXISTS posts_scheduled;
ALTER TABLE posts DROP COLUMN cover;
ALTER TABLE posts DROP COLUMN scheduled_at;
