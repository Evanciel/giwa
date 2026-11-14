"""관리 화면 테마·스킨 카드의 실제 화면 썸네일을 만든다 (ADR-0141).

프리셋마다 사이트를 만들고(`init` 이 예시 글과 그림을 넣는다, ADR-0142) 서버를 띄워 헤드리스
크롬으로 첫 화면을 찍는다. 스킨은 같은 예시 사이트(회사 소개)에 스킨만 바꿔 입혀 찍는다.

    python scripts/theme_shots.py --bin <giwa.exe> --work <작업 폴더> [--port 3941] [--only shop,clinic]

결과: crates/themes/admin/static/shots/site-<프리셋>.webp, skin-<스킨>.webp
프리셋이나 기본 테마 CSS 가 바뀌면 다시 돌려 커밋한다. 개발 도구라 Pillow 와 크롬(또는 엣지)이 필요하다.
"""

import argparse
import json
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from PIL import Image

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


def giwa(bin_, data, *args):
    out = subprocess.run(
        [bin_, "--data", str(data), *args, "--json"],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    try:
        res = json.loads(out.stdout)
    except json.JSONDecodeError:
        sys.exit(f"giwa {' '.join(args)}: {out.stdout} {out.stderr}")
    if res.get("ok") is False:
        sys.exit(f"giwa {' '.join(args)}: {res}")
    return res.get("data", res)


def seed(bin_, data, name):
    giwa(bin_, data, "site", "set", "--name", name)


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
    # 출력을 잡지 않는다: 남은 크롬 자식 프로세스가 파이프를 쥐면 끝난 뒤에도 기다린다.
    # 바쁜 PC 에서는 가끔 멈추므로 세 번까지 다시 찍는다.
    for _ in range(3):
        try:
            subprocess.run(
                [browser, "--headless=new", "--disable-gpu", "--hide-scrollbars",
                 "--force-color-profile=srgb", "--force-prefers-reduced-motion",
                 f"--user-data-dir={work / 'browser'}", f"--window-size={SHOT_W},{SHOT_H}",
                 "--virtual-time-budget=4000", f"--screenshot={png}",
                 f"http://127.0.0.1:{port}/"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=120,
            )
        except subprocess.TimeoutExpired:
            pass
        if png.exists():
            break
    if not png.exists():
        sys.exit("화면을 찍지 못했다")
    img = Image.open(png).convert("RGB").crop((0, 0, SHOT_W, SHOT_H))
    img = img.resize((THUMB_W, THUMB_H), Image.LANCZOS)
    img.save(out, "WEBP", quality=78, method=6)
    print(out.relative_to(ROOT), out.stat().st_size // 1024, "KB")


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
        giwa(a.bin, data, "init", "--preset", pid, "--yes")
        seed(a.bin, data, preset_name(pid))
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
    giwa(a.bin, plain, "init", "--no-samples", "--yes")
    base = giwa(a.bin, plain, "theme", "tokens", "get")
    default = w / "default.json"
    default.write_text(
        json.dumps({k: base[k] for k in ("color", "color_dark", "radius", "font") if k in base}),
        encoding="utf-8",
    )
    data = w / "data"
    giwa(a.bin, data, "init", "--preset", SKIN_DEMO, "--yes")
    seed(a.bin, data, "회사 이름")
    themes = [{"id": "default"}] + items(giwa(a.bin, data, "theme", "list"))
    for t in themes:
        tid = t["id"]
        if only and tid not in only:
            continue
        if tid == "default":
            giwa(a.bin, data, "theme", "tokens", "set", "--from", str(default), "--yes")
        else:
            giwa(a.bin, data, "theme", "apply", tid, "--yes")
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
