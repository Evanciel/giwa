-- 게시판 즐겨찾기 (GAP Q26, ADR-0108). 코어 표를 참조하지 않는다: 지운 게시판은 읽을 때 건너뛴다.
CREATE TABLE mod_scrap_board (
    member_id TEXT NOT NULL,
    board_id  TEXT NOT NULL,
    at        TEXT NOT NULL,
    PRIMARY KEY (member_id, board_id)
) STRICT, WITHOUT ROWID;
