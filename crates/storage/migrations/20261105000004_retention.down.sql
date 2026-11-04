UPDATE members SET email = (SELECT d.email FROM member_dormant d WHERE d.member_id = members.id)
WHERE id IN (SELECT member_id FROM member_dormant);
DROP TABLE member_dormant;
ALTER TABLE members DROP COLUMN dormant_notice_at;
ALTER TABLE members DROP COLUMN last_login_at;
