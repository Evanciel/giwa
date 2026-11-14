# 나눔 폰트를 한글 11172자 + 호환 자모 + 영문·숫자·문장부호로 줄여 woff2 로 쓴다(힌팅 제거).
# OFL 예약 글꼴 이름(Nanum…) 때문에 줄인 파일의 이름표를 Giwa … 로 바꾼다.
# 쓰는 법: python scripts/subset-fonts.py <google/fonts 의 ofl 폴더처럼 받은 곳> <출력 폴더>  (fonttools·brotli 필요)
# 결과는 crates/themes/builtin/static/fonts/ 에 둔다. 파일 이름의 -1 은 내용이 바뀌면 올린다(오래 캐시).
import sys
from fontTools import subset
from fontTools.ttLib import TTFont

SRC, OUT = sys.argv[1], sys.argv[2]  # 원본 TTF 폴더, 출력 폴더
JOBS = [
    ("nanumpenscript/NanumPenScript-Regular.ttf", "maru-pen-1.woff2", "Giwa Pen"),
    ("nanumbrushscript/NanumBrushScript-Regular.ttf", "maru-brush-1.woff2", "Giwa Brush"),
    ("nanummyeongjo/NanumMyeongjo-Regular.ttf", "maru-serif-1.woff2", "Giwa Serif"),
]

chars = set(range(0x20, 0x7F))
chars |= set(range(0x3131, 0x318F))  # 호환 자모
for cp in range(0xAC00, 0xD7A4):  # euc-kr 코덱은 조합 시퀀스로 11172자를 모두 받는다
    try:
        chr(cp).encode("euc-kr")
        chars.add(cp)
    except UnicodeEncodeError:
        pass
chars |= {0xA0, 0xB7, 0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2026, 0x203B,
          0x2190, 0x2191, 0x2192, 0x2193, 0x3001, 0x3002, 0x3008, 0x3009, 0x300A, 0x300B,
          0x300C, 0x300D, 0x300E, 0x300F, 0x3010, 0x3011, 0xFF5E, 0x223C}
hangul = sum(1 for c in chars if 0xAC00 <= c <= 0xD7A3)
print("hangul", hangul, "total", len(chars))

RENAME = (1, 3, 4, 6, 16, 17, 18, 21, 22)
for src, out, family in JOBS:
    font = TTFont(f"{SRC}/{src}")
    opts = subset.Options()
    opts.flavor = "woff2"
    opts.layout_features = ["*"]
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.hinting = False
    sub = subset.Subsetter(opts)
    sub.populate(unicodes=chars)
    sub.subset(font)
    name = font["name"]
    name.names = [n for n in name.names if n.nameID not in RENAME]
    ps = family.replace(" ", "") + "-Regular"
    for nid, val in ((1, family), (2, "Regular"), (3, f"{ps};maru-subset-1"), (4, f"{family} Regular"), (6, ps)):
        name.setName(val, nid, 3, 1, 0x409)
    font.flavor = "woff2"
    font.save(f"{OUT}/{out}")
    left = [n.toUnicode() for n in name.names if "Nanum" in n.toUnicode() and n.nameID not in (0, 13, 14)]
    print(out, "nanum-left", left)
