-- 게시판 안내 문구 (GAP Q9, ADR-0094). 목록 위에 보이는 평문. 빈 값이면 보이지 않는다.
ALTER TABLE boards ADD COLUMN description TEXT NOT NULL DEFAULT '';
