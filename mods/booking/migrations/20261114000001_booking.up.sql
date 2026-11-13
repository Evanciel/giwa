-- 예약 모듈 (ADR-0169). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
-- 날짜(day)와 분(start_min·end_min)은 사이트 시간대 기준, starts_at 만 UTC 다.

-- 예약받는 것 (진료·상담·수업·객실). 금액은 원 단위 정수.
CREATE TABLE mod_booking_service (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    minutes     INTEGER NOT NULL CHECK (minutes BETWEEN 10 AND 720),
    capacity    INTEGER NOT NULL DEFAULT 1 CHECK (capacity BETWEEN 1 AND 500),
    price       INTEGER NOT NULL DEFAULT 0 CHECK (price >= 0),
    deposit     INTEGER NOT NULL DEFAULT 0 CHECK (deposit >= 0),
    enabled     INTEGER NOT NULL DEFAULT 1,
    position    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

-- 요일별 영업 시간. 0 = 월요일. 줄이 없으면 쉬는 요일이다.
CREATE TABLE mod_booking_hours (
    weekday   INTEGER PRIMARY KEY CHECK (weekday BETWEEN 0 AND 6),
    open_min  INTEGER NOT NULL,
    close_min INTEGER NOT NULL,
    CHECK (open_min >= 0 AND open_min < close_min AND close_min <= 1440)
) STRICT;

-- 막은 시간. 0~1440 이면 하루 쉬는 날이다.
CREATE TABLE mod_booking_block (
    id         TEXT PRIMARY KEY,
    day        TEXT NOT NULL,
    start_min  INTEGER NOT NULL,
    end_min    INTEGER NOT NULL,
    reason     TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    CHECK (start_min >= 0 AND start_min < end_min AND end_min <= 1440)
) STRICT;
CREATE INDEX mod_booking_block_day ON mod_booking_block (day);

-- 예약. held 는 예약금 결제를 기다리며 hold_until 까지 자리를 잡는다.
CREATE TABLE mod_booking_booking (
    id            TEXT PRIMARY KEY,
    service_id    TEXT NOT NULL,
    service_name  TEXT NOT NULL,
    day           TEXT NOT NULL,
    start_min     INTEGER NOT NULL,
    end_min       INTEGER NOT NULL,
    starts_at     TEXT NOT NULL,
    status        TEXT NOT NULL,
    deposit       INTEGER NOT NULL DEFAULT 0,
    -- 결제 모듈 주문 (Checkout). 이 모듈은 주문 ID 만 안다.
    order_id      TEXT,
    paid          INTEGER NOT NULL DEFAULT 0,
    refunded      INTEGER NOT NULL DEFAULT 0,
    hold_until    TEXT,
    name          TEXT NOT NULL DEFAULT '',
    email         TEXT NOT NULL DEFAULT '',
    phone         TEXT NOT NULL DEFAULT '',
    memo          TEXT NOT NULL DEFAULT '',
    member_id     TEXT,
    -- 손님 취소 링크 1회용 값의 SHA-256 (ADR-0009). 쓰면 지운다.
    cancel_hash   TEXT,
    cancel_reason TEXT NOT NULL DEFAULT '',
    canceled_by   TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;
CREATE INDEX mod_booking_booking_slot ON mod_booking_booking (service_id, day, status);
CREATE INDEX mod_booking_booking_day ON mod_booking_booking (day, start_min);
CREATE INDEX mod_booking_booking_hold ON mod_booking_booking (status, hold_until);
CREATE UNIQUE INDEX mod_booking_booking_order ON mod_booking_booking (order_id) WHERE order_id IS NOT NULL;
