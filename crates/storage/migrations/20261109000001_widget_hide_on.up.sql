-- 기기별 숨김 (ADR-0146). 쉼표로 이은 기기 이름(desktop, tablet, mobile). 비면 모든 기기에 보인다.
ALTER TABLE widgets ADD COLUMN hide_on TEXT NOT NULL DEFAULT '';
