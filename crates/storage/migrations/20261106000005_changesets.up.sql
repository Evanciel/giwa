-- 변경 묶음 (SPEC §6, ADR-0121). ops·inverse 는 연산 JSON 배열.
CREATE TABLE changesets (
    id          TEXT PRIMARY KEY NOT NULL,
    title       TEXT NOT NULL,
    actor_type  TEXT NOT NULL,
    actor_id    TEXT,
    status      TEXT NOT NULL CHECK (status IN ('preview', 'applying', 'applied', 'failed', 'discarded', 'reverted')),
    ops         TEXT NOT NULL,
    inverse     TEXT NOT NULL,
    error       TEXT,
    created_at  TEXT NOT NULL,
    applied_at  TEXT,
    reverted_at TEXT
) STRICT;
CREATE INDEX changesets_status ON changesets (status, id);
ALTER TABLE audit_log ADD COLUMN changeset_id TEXT;
CREATE INDEX audit_log_changeset ON audit_log (changeset_id) WHERE changeset_id IS NOT NULL;
