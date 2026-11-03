-- 첨부 파일 (PDF·문서·압축 등). 그림과 달리 다시 인코딩하지 않고 내려받기로만 제공한다.
-- filename 은 화면·다운로드 이름용이고 보관 경로에는 쓰지 않는다.
CREATE TABLE files (
    id         TEXT PRIMARY KEY NOT NULL,
    path       TEXT NOT NULL UNIQUE,
    filename   TEXT NOT NULL,
    mime       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    created_at TEXT NOT NULL
) STRICT;
CREATE INDEX files_created ON files (created_at);
