-- 비회원 글 (ADR-0053). 손님도 사람이라 actor_type 은 'human', author_id 는 비운다.
-- 비밀번호는 argon2id 해시만. 이름·해시는 둘 다 있거나 둘 다 없다(서비스가 지킨다).
ALTER TABLE posts ADD COLUMN guest_name TEXT;
ALTER TABLE posts ADD COLUMN guest_pw_hash TEXT;
