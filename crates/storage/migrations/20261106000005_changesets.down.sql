DROP INDEX audit_log_changeset;
ALTER TABLE audit_log DROP COLUMN changeset_id;
DROP TABLE changesets;
