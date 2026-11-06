-- 1회용 미리보기 토큰 (SPEC §6, ADR-0122). SHA-256 해시만 둔다.
ALTER TABLE changesets ADD COLUMN preview_token_hash TEXT;
ALTER TABLE changesets ADD COLUMN preview_token_expires TEXT;
