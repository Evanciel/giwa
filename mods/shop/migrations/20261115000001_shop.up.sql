-- 쇼핑 모듈 (ADR-0170). 상품·장바구니·주문. 금액은 원 단위 정수.
CREATE TABLE mod_shop_product (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    price INTEGER NOT NULL CHECK (price >= 0),
    -- NULL 이면 재고를 세지 않는다(무제한)
    stock INTEGER CHECK (stock IS NULL OR stock >= 0),
    image TEXT NOT NULL DEFAULT '',
    -- 옵션 한 단계: 이름(크기·색)과 쉼표로 나눈 값들
    option_label TEXT NOT NULL DEFAULT '',
    options TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    position INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- 손님 장바구니. 쿠키 값은 SHA-256 만 남긴다(ADR-0009). 30일 지나면 지운다.
CREATE TABLE mod_shop_cart (
    id TEXT PRIMARY KEY NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    member_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE mod_shop_cart_item (
    cart_id TEXT NOT NULL REFERENCES mod_shop_cart (id) ON DELETE CASCADE,
    product_id TEXT NOT NULL,
    option TEXT NOT NULL DEFAULT '',
    qty INTEGER NOT NULL CHECK (qty BETWEEN 1 AND 99),
    added_at TEXT NOT NULL,
    PRIMARY KEY (cart_id, product_id, option)
);

-- 주문. pending → paid → preparing → shipped → delivered, 또는 canceled·expired.
CREATE TABLE mod_shop_order (
    id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL,
    pay_order_id TEXT,
    cart_id TEXT,
    subtotal INTEGER NOT NULL,
    shipping INTEGER NOT NULL,
    total INTEGER NOT NULL,
    -- 결제 완료 때 재고를 줄였는가 / 모자라 못 줄였는가
    stock_taken INTEGER NOT NULL DEFAULT 0,
    short INTEGER NOT NULL DEFAULT 0,
    refunded INTEGER NOT NULL DEFAULT 0,
    name TEXT NOT NULL DEFAULT '',
    email TEXT NOT NULL DEFAULT '',
    phone TEXT NOT NULL DEFAULT '',
    zip TEXT NOT NULL DEFAULT '',
    address TEXT NOT NULL DEFAULT '',
    address2 TEXT NOT NULL DEFAULT '',
    memo TEXT NOT NULL DEFAULT '',
    member_id TEXT,
    carrier TEXT NOT NULL DEFAULT '',
    tracking TEXT NOT NULL DEFAULT '',
    cancel_reason TEXT NOT NULL DEFAULT '',
    paid_at TEXT,
    shipped_at TEXT,
    delivered_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX mod_shop_order_pay ON mod_shop_order (pay_order_id) WHERE pay_order_id IS NOT NULL;
CREATE INDEX mod_shop_order_status ON mod_shop_order (status, created_at);
CREATE INDEX mod_shop_order_member ON mod_shop_order (member_id, created_at);

-- 주문 때의 이름·옵션·가격을 그대로 남긴다(상품을 고쳐도 지난 주문은 그대로).
CREATE TABLE mod_shop_order_item (
    order_id TEXT NOT NULL REFERENCES mod_shop_order (id) ON DELETE CASCADE,
    line INTEGER NOT NULL,
    product_id TEXT NOT NULL,
    name TEXT NOT NULL,
    option TEXT NOT NULL DEFAULT '',
    price INTEGER NOT NULL,
    qty INTEGER NOT NULL,
    PRIMARY KEY (order_id, line)
);
