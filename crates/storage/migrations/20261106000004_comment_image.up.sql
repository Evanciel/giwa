-- 댓글에 붙인 그림 한 장의 주소 (`/media/…`, GAP Q8, ADR-0116).
ALTER TABLE comments ADD COLUMN image TEXT;
