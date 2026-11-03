DROP INDEX IF EXISTS posts_notice;
DROP INDEX IF EXISTS posts_category;
ALTER TABLE posts DROP COLUMN is_secret;
ALTER TABLE posts DROP COLUMN is_notice;
ALTER TABLE posts DROP COLUMN category_id;
DROP INDEX IF EXISTS post_tags_tag;
DROP TABLE IF EXISTS post_tags;
DROP TABLE IF EXISTS tags;
DROP TABLE IF EXISTS categories;
