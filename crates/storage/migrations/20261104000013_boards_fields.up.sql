-- 여분 필드 (ADR-0057). 게시판은 칸 정의 배열, 글은 키 → 값 객체.
ALTER TABLE boards ADD COLUMN field_schema TEXT NOT NULL DEFAULT '[]';
ALTER TABLE posts ADD COLUMN meta TEXT NOT NULL DEFAULT '{}';
