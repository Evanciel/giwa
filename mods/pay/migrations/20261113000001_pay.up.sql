-- 결제 모듈 (ADR-0166). 코어 표를 참조하지 않는다: 모듈을 지워도 코어가 그대로다.
-- 결제 방법 설정. 비밀 키는 화면·JSON·감사 기록에 끝 네 글자만 나간다.
CREATE TABLE mod_pay_provider (
    id         TEXT PRIMARY KEY,
    enabled    INTEGER NOT NULL DEFAULT 0,
    client_key TEXT NOT NULL DEFAULT '',
    secret_key TEXT NOT NULL DEFAULT '',
    config     TEXT NOT NULL DEFAULT '{}',
    updated_at TEXT NOT NULL
) STRICT;

-- 주문 하나에 결제 하나. 금액은 원 단위 정수.
CREATE TABLE mod_pay_order (
    id             TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    ref_id         TEXT NOT NULL DEFAULT '',
    name           TEXT NOT NULL,
    amount         INTEGER NOT NULL CHECK (amount > 0),
    refunded       INTEGER NOT NULL DEFAULT 0 CHECK (refunded >= 0 AND refunded <= amount),
    status         TEXT NOT NULL,
    provider       TEXT NOT NULL DEFAULT '',
    -- 결제사에 보낸 주문 번호 (`<id>-<시도>`). 결제 창을 다시 열 때마다 바뀐다.
    pg_order_id    TEXT,
    attempts       INTEGER NOT NULL DEFAULT 0,
    payment_key    TEXT,
    method         TEXT NOT NULL DEFAULT '',
    -- 가상계좌 입금 알림을 확인하는 값 (토스 승인 응답의 secret)
    deposit_secret TEXT,
    last_error     TEXT NOT NULL DEFAULT '',
    buyer_name     TEXT NOT NULL DEFAULT '',
    buyer_email    TEXT NOT NULL DEFAULT '',
    buyer_phone    TEXT NOT NULL DEFAULT '',
    depositor      TEXT NOT NULL DEFAULT '',
    member_id      TEXT,
    return_to      TEXT NOT NULL DEFAULT '',
    -- 구매자 주문 화면 1회용 값의 SHA-256 (ADR-0009)
    view_hash      TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    paid_at        TEXT,
    updated_at     TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX mod_pay_order_pg ON mod_pay_order (pg_order_id) WHERE pg_order_id IS NOT NULL;
CREATE UNIQUE INDEX mod_pay_order_key ON mod_pay_order (payment_key) WHERE payment_key IS NOT NULL;
CREATE INDEX mod_pay_order_status ON mod_pay_order (status, created_at);
CREATE INDEX mod_pay_order_owner ON mod_pay_order (owner, ref_id);

-- 환불. id 가 결제사 멱등 키다.
CREATE TABLE mod_pay_refund (
    id         TEXT PRIMARY KEY,
    order_id   TEXT NOT NULL REFERENCES mod_pay_order (id) ON DELETE CASCADE,
    amount     INTEGER NOT NULL CHECK (amount > 0),
    reason     TEXT NOT NULL,
    status     TEXT NOT NULL,
    error      TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    done_at    TEXT
) STRICT;
CREATE INDEX mod_pay_refund_order ON mod_pay_refund (order_id, created_at);
