-- 글 틀: 새 글을 쓸 때 불러오는 본문 뼈대. 기본 틀은 바이너리에 있고 여기에는 사용자가 저장한 것만 둔다.
CREATE TABLE post_templates (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL UNIQUE,
    body_md    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;
