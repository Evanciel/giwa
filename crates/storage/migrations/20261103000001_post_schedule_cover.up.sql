-- 예약 발행(scheduled_at)과 대표 이미지(cover). 둘 다 비어 있을 수 있다.
-- 예약 시각이 지난 초안·검토·숨김 글은 서버가 주기적으로 발행한다.
ALTER TABLE posts ADD COLUMN scheduled_at TEXT;
ALTER TABLE posts ADD COLUMN cover TEXT;
CREATE INDEX posts_scheduled ON posts (scheduled_at) WHERE scheduled_at IS NOT NULL;
