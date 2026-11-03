-- 게시판 목록 형태 (ADR-0054). 빈 값은 게시판 종류의 기본값.
ALTER TABLE boards ADD COLUMN layout TEXT NOT NULL DEFAULT '';
