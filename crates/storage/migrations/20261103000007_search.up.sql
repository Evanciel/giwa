-- 사이트 검색 (GAP C5, ADR-0050).
-- 색인 글자는 core 가 한글을 2글자 단위로 잘라 넣는다(SQL 로는 못 자르므로 마이그레이션은 빈 표만 만든다).
-- 서버·CLI 가 DB 를 열 때 search_state.version 이 core 의 INDEX_VERSION 과 다르면 전부 다시 만든다.
-- 색인에는 내용만 넣고, 공개 여부·비밀글·게시판 권한은 찾을 때 posts/pages 와 이어 붙여 거른다.

-- FTS5 rowid 를 고정하는 표. posts/pages 의 암묵 rowid 는 VACUUM 으로 바뀔 수 있어 쓰지 않는다.
CREATE TABLE search_docs (
    id      INTEGER PRIMARY KEY,
    post_id TEXT UNIQUE REFERENCES posts (id) ON DELETE CASCADE,
    page_id TEXT UNIQUE REFERENCES pages (id) ON DELETE CASCADE,
    CHECK ((post_id IS NULL) <> (page_id IS NULL))
);

CREATE VIRTUAL TABLE search_fts USING fts5 (
    title, body, tags,
    tokenize = 'unicode61 remove_diacritics 0'
);

CREATE TABLE search_state (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    version INTEGER NOT NULL
);
INSERT INTO search_state (id, version) VALUES (1, 0);
