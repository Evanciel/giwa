-- 출석체크 (GAP Q10, ADR-0095). 회원마다 하루 한 줄. `day` 는 한국 시간 날짜(YYYY-MM-DD),
-- `streak` 는 그날까지 연달아 출석한 날수. 코어 표를 참조하지 않는다.
CREATE TABLE mod_point_attend (
    member_id  TEXT NOT NULL,
    day        TEXT NOT NULL,
    streak     INTEGER NOT NULL,
    at         TEXT NOT NULL,
    PRIMARY KEY (member_id, day)
) STRICT;
CREATE INDEX mod_point_attend_day ON mod_point_attend (day, at, member_id);
