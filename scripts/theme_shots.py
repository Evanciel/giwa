"""관리 화면 테마·스킨 카드의 실제 화면 썸네일을 만든다 (ADR-0141).

프리셋마다 빈 사이트를 만들고 게시판마다 「예시 글」을 몇 개 넣은 뒤, 서버를 띄워 헤드리스
크롬으로 첫 화면을 찍는다. 스킨은 같은 예시 사이트(회사 소개)에 스킨만 바꿔 입혀 찍는다.
그림은 이 대본이 색 면과 도형으로 그린 것만 쓴다(남의 사진·그림을 쓰지 않는다, 원칙 7).

    python scripts/theme_shots.py --bin <maru.exe> --work <작업 폴더> [--port 3941] [--only shop,clinic]

결과: crates/themes/admin/static/shots/site-<프리셋>.webp, skin-<스킨>.webp
프리셋이나 기본 테마 CSS 가 바뀌면 다시 돌려 커밋한다. 개발 도구라 Pillow 와 크롬(또는 엣지)이 필요하다.
"""

import argparse
import json
import random
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates" / "themes" / "admin" / "static" / "shots"
PRESETS = ROOT / "crates" / "themes" / "builtin" / "presets"
SHOT_W, SHOT_H = 1280, 2400
THUMB_W, THUMB_H = 640, 1200
SKIN_DEMO = "company"
BROWSERS = [
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
]


def maru(bin_, data, *args):
    out = subprocess.run(
        [bin_, "--data", str(data), *args, "--json"],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    try:
        res = json.loads(out.stdout)
    except json.JSONDecodeError:
        sys.exit(f"maru {' '.join(args)}: {out.stdout} {out.stderr}")
    if res.get("ok") is False:
        sys.exit(f"maru {' '.join(args)}: {res}")
    return res.get("data", res)


def sample(field):
    t = field.get("type", "text")
    if t in ("select", "multiselect"):
        return field["options"][0]["value"]
    return {
        "number": "10",
        "boolean": "false",
        "date": "2026-12-20",
        "datetime": "2026-12-20T10:00",
        "url": "https://example.com",
        "email": "hello@example.com",
        "phone": "000-0000-0000",
    }.get(t, "예시")


def hex_rgb(h, fallback=(47, 93, 80)):
    h = (h or "").lstrip("#")
    if len(h) != 6:
        return fallback
    return tuple(int(h[i : i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def make_images(work, primary, n=4):
    """색 면과 도형만으로 그린 예시 그림. 사진처럼 보이게 꾸미지 않는다."""
    paths = []
    rnd = random.Random(7)
    for i in range(n):
        img = Image.new("RGB", (960, 640), mix(primary, (255, 255, 255), 0.82))
        d = ImageDraw.Draw(img)
        for _ in range(3):
            c = mix(primary, (255, 255, 255), rnd.choice([0.15, 0.35, 0.55]))
            x, y, r = rnd.randint(80, 880), rnd.randint(60, 580), rnd.randint(90, 260)
            if rnd.random() < 0.5:
                d.ellipse((x - r, y - r, x + r, y + r), fill=c)
            else:
                d.rounded_rectangle((x - r, y - r // 2, x + r, y + r // 2), radius=40, fill=c)
        p = work / f"img-{i}.png"
        img.save(p)
        paths.append(p)
    return paths


def seed(bin_, data, name, primary, work):
    maru(bin_, data, "site", "set", "--name", name)
    media = []
    for p in make_images(work, primary):
        m = maru(bin_, data, "media", "upload", str(p), "--alt", "예시 그림", "--yes")
        media.append(m["url"])
    boards = items(maru(bin_, data, "board", "list"))
    for b in boards:
        visual = b.get("kind") == "gallery" or b.get("layout") in ("gallery", "card")
        for i in range(1, 4):
            body = "예시 본문입니다. 실제 글을 올리면 이 자리에 보입니다."
            if visual or i == 1:
                body = f"![예시 그림]({media[(i - 1) % len(media)]})\n\n{body}"
            args = ["post", "new", "--publish", "--board", b["slug"],
                    "--title", f"{b['name']} 예시 글 {i}", "--body", body, "--yes"]
            for f in b.get("field_schema") or []:
                if f.get("required") or f.get("list"):
                    args += ["--field", f"{f['key']}={sample(f)}"]
            maru(bin_, data, *args)


def serve(bin_, data, port):
    proc = subprocess.Popen(
        [bin_, "--data", str(data), "serve", "--bind", f"127.0.0.1:{port}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(100):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1)
            return proc
        except Exception:
            time.sleep(0.2)
    proc.kill()
    sys.exit("서버가 뜨지 않았다")


def shoot(browser, port, work, out):
    png = work / "shot.png"
    png.unlink(missing_ok=True)
    subprocess.run(
        [browser, "--headless=new", "--disable-gpu", "--hide-scrollbars",
         "--force-color-profile=srgb", "--force-prefers-reduced-motion",
         f"--user-data-dir={work / 'browser'}", f"--window-size={SHOT_W},{SHOT_H}",
         "--virtual-time-budget=4000", f"--screenshot={png}", f"http://127.0.0.1:{port}/"],
        capture_output=True,
        timeout=120,
    )
    if not png.exists():
        sys.exit("화면을 찍지 못했다")
    img = Image.open(png).convert("RGB").crop((0, 0, SHOT_W, SHOT_H))
    img = img.resize((THUMB_W, THUMB_H), Image.LANCZOS)
    img.save(out, "WEBP", quality=78, method=6)
    print(out.relative_to(ROOT), out.stat().st_size // 1024, "KB")


def primary_of(bin_, data):
    tokens = maru(bin_, data, "theme", "tokens", "get")
    return hex_rgb((tokens.get("color") or {}).get("primary"))


def items(res):
    return res.get("items", []) if isinstance(res, dict) else res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--port", type=int, default=3941)
    ap.add_argument("--only", default="")
    ap.add_argument("--browser", default="")
    a = ap.parse_args()
    browser = a.browser or next((b for b in BROWSERS if Path(b).exists()), "")
    if not browser:
        sys.exit("크롬이나 엣지를 --browser 로 알려 주세요")
    work = Path(a.work)
    OUT.mkdir(parents=True, exist_ok=True)
    only = {x for x in a.only.split(",") if x}

    ids = sorted(p.stem for p in PRESETS.glob("*.toml"))
    for pid in ids:
        if only and pid not in only:
            continue
        w = work / f"site-{pid}"
        shutil.rmtree(w, ignore_errors=True)
        w.mkdir(parents=True)
        data = w / "data"
        maru(a.bin, data, "init", "--preset", pid, "--yes")
        seed(a.bin, data, preset_name(pid), primary_of(a.bin, data), w)
        proc = serve(a.bin, data, a.port)
        try:
            shoot(browser, a.port, w, OUT / f"site-{pid}.webp")
        finally:
            proc.kill()
            proc.wait()

    # 스킨: 같은 예시 사이트에 스킨만 바꿔 입힌다. 「기본」은 토큰을 안 건드린 사이트의 값이다.
    w = work / "skins"
    shutil.rmtree(w, ignore_errors=True)
    w.mkdir(parents=True)
    plain = w / "plain"
    maru(a.bin, plain, "init", "--yes")
    base = maru(a.bin, plain, "theme", "tokens", "get")
    default = w / "default.json"
    default.write_text(
        json.dumps({k: base[k] for k in ("color", "color_dark", "radius", "font") if k in base}),
        encoding="utf-8",
    )
    data = w / "data"
    maru(a.bin, data, "init", "--preset", SKIN_DEMO, "--yes")
    seed(a.bin, data, "회사 이름", primary_of(a.bin, data), w)
    themes = [{"id": "default"}] + items(maru(a.bin, data, "theme", "list"))
    for t in themes:
        tid = t["id"]
        if only and tid not in only:
            continue
        if tid == "default":
            maru(a.bin, data, "theme", "tokens", "set", "--from", str(default), "--yes")
        else:
            maru(a.bin, data, "theme", "apply", tid, "--yes")
        proc = serve(a.bin, data, a.port)
        try:
            shoot(browser, a.port, w, OUT / f"skin-{tid}.webp")
        finally:
            proc.kill()
            proc.wait()


def preset_name(pid):
    for line in (PRESETS / f"{pid}.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("name = "):
            return line.split("=", 1)[1].strip().strip('"')
    return pid


if __name__ == "__main__":
    main()
