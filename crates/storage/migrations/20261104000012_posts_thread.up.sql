-- 답변글 (ADR-0056). parent_id 는 바로 위 글, thread_id 는 맨 위 글(맨 위 글은 비움),
-- thread_path 는 형제 순서 `0001.0002.`, thread_owner 는 맨 위 글을 쓴 회원(비밀 답변 읽기).
ALTER TABLE posts ADD COLUMN parent_id TEXT REFERENCES posts (id);
ALTER TABLE posts ADD COLUMN thread_id TEXT;
ALTER TABLE posts ADD COLUMN thread_path TEXT NOT NULL DEFAULT '';
ALTER TABLE posts ADD COLUMN thread_owner TEXT;
CREATE INDEX posts_parent ON posts (parent_id);
CREATE INDEX posts_thread ON posts (thread_id);
