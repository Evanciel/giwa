DROP INDEX mail_outbox_status;
DROP TABLE mail_outbox;
ALTER TABLE login_tickets DROP COLUMN purpose;
DROP INDEX members_display_name;
ALTER TABLE members DROP COLUMN withdrawn_at;
ALTER TABLE members DROP COLUMN email_verified_at;
