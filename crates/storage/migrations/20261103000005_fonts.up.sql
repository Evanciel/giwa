-- 운영자가 올린 서체 (docs/plans/font-upload.md, ADR-0048).
-- 서체 하나에 굵기·기울임별 파일 여러 개. 파일 경로는 내용 해시라 바꾸면 URL 도 바뀐다.

CREATE TABLE fonts (
    id           TEXT PRIMARY KEY NOT NULL,
    slug         TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    fallback     TEXT NOT NULL DEFAULT 'sans',
    license      TEXT NOT NULL,
    license_path TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;

CREATE TABLE font_files (
    id         TEXT PRIMARY KEY NOT NULL,
    font_id    TEXT NOT NULL REFERENCES fonts (id) ON DELETE CASCADE,
    weight     INTEGER NOT NULL DEFAULT 400 CHECK (weight BETWEEN 100 AND 900),
    italic     INTEGER NOT NULL DEFAULT 0,
    format     TEXT NOT NULL,
    path       TEXT NOT NULL UNIQUE,
    size       INTEGER NOT NULL,
    sha256     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (font_id, weight, italic)
) STRICT;
