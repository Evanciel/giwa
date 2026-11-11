-- 페이지 검색 설명·공유 그림·검색에서 빼기 (ADR-0160). 비어 있으면 지금처럼 본문 앞부분과 사이트 공유 그림을 쓴다.
ALTER TABLE pages ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE pages ADD COLUMN share_image TEXT NOT NULL DEFAULT '';
ALTER TABLE pages ADD COLUMN noindex INTEGER NOT NULL DEFAULT 0 CHECK (noindex IN (0, 1));
