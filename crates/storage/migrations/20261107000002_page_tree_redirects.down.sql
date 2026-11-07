DROP TABLE redirects;
DROP INDEX idx_pages_parent;
ALTER TABLE pages DROP COLUMN parent_id;
