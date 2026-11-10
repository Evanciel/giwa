-- 화면 편집기 초안 (ADR-0152). 사람마다 화면마다 한 벌. 발행하거나 버리면 지운다
CREATE TABLE editor_drafts (
    member_id  TEXT NOT NULL REFERENCES members (id) ON DELETE CASCADE,
    screen     TEXT NOT NULL,
    body       TEXT NOT NULL,
    base       TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (member_id, screen)
) STRICT, WITHOUT ROWID;
CREATE INDEX editor_drafts_screen ON editor_drafts (screen, updated_at);
