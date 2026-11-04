-- 도배 방지가 회원의 마지막 글을 찾는다 (ADR-0071).
CREATE INDEX posts_author ON posts (author_id, created_at);
