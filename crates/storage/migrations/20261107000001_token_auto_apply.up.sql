-- 사람 승인 없이 changeset 을 적용해도 되는 토큰 (SPEC §5.3, ADR-0123).
ALTER TABLE tokens ADD COLUMN auto_apply INTEGER NOT NULL DEFAULT 0 CHECK (auto_apply IN (0, 1));
