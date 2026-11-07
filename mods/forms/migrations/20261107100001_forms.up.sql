-- 폼 모듈 (ADR-0127). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
CREATE TABLE mod_forms_submission (
    id      TEXT PRIMARY KEY,
    form    TEXT NOT NULL,
    data    TEXT NOT NULL,
    at      TEXT NOT NULL,
    read_at TEXT
) STRICT;
CREATE INDEX mod_forms_submission_form ON mod_forms_submission (form, at);
CREATE INDEX mod_forms_submission_at ON mod_forms_submission (at);
