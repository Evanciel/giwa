//! 예시 글에 넣는 그림 (ADR-0142). 사진이 아니라 사이트 주 색으로 칠한 색 면과 도형의 장면이다.
//! 남의 사진·그림을 쓰지 않고(원칙 7), 바이너리에 그림 파일을 싣지 않는다.
//! 도형은 거리 함수로 그려 가장자리를 부드럽게 한다. 같은 (장면, 번호, 색)이면 같은 그림이 나온다.

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use image::{Rgb, RgbImage};

use crate::error::{Error, Result};

pub const WIDTH: u32 = 960;
pub const HEIGHT: u32 = 640;
const JPEG_QUALITY: u8 = 84;

/// 고를 수 있는 장면. 프리셋 예시 글의 `image` 값.
pub const MOTIFS: [&str; 8] = [
    "landscape",
    "still",
    "room",
    "city",
    "food",
    "desk",
    "people",
    "abstract",
];

pub fn is_motif(name: &str) -> bool {
    MOTIFS.contains(&name)
}

/// JPEG 바이트. `primary` 는 `#rrggbb` (틀리면 녹색 계열로).
pub fn render(motif: &str, seed: u32, primary: &str) -> Result<Vec<u8>> {
    draw(motif, seed, primary, WIDTH, HEIGHT)
}

/// 공유 미리보기(`og:image`) 크기.
pub const SHARE_WIDTH: u32 = 1200;
pub const SHARE_HEIGHT: u32 = 630;

/// 사이트 공유 그림 (ADR-0143): 사이트 주 색의 추상 장면, 1200×630 JPEG.
pub fn share(seed: u32, primary: &str) -> Result<Vec<u8>> {
    draw("abstract", seed, primary, SHARE_WIDTH, SHARE_HEIGHT)
}

fn draw(motif: &str, seed: u32, primary: &str, w: u32, h: u32) -> Result<Vec<u8>> {
    let mut c = Canvas::new(w, h);
    let p = Palette::from_hex(primary);
    let mut r = Lcg(seed
        .wrapping_mul(2_654_435_761)
        .wrapping_add(motif.len() as u32 * 97));
    match motif {
        "landscape" => landscape(&mut c, &p, &mut r),
        "still" => still(&mut c, &p, &mut r),
        "room" => room(&mut c, &p, &mut r),
        "city" => city(&mut c, &p, &mut r),
        "food" => food(&mut c, &p, &mut r),
        "desk" => desk(&mut c, &p, &mut r),
        "people" => people(&mut c, &p, &mut r),
        _ => abstract_(&mut c, &p, &mut r),
    }
    let img = c.into_image();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&img)
        .map_err(Error::internal)?;
    Ok(out)
}

// ---- 장면 ----

fn landscape(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.vgradient(p.tint(0.86), p.tint(0.97));
    let sx = r.range(0.2, 0.8) * c.w;
    c.circle(
        sx,
        c.h * r.range(0.22, 0.34),
        c.h * 0.09,
        p.accent_tint(0.55),
        1.0,
    );
    for (i, t) in [0.62, 0.42, 0.18].iter().enumerate() {
        let base = c.h * (0.48 + 0.14 * i as f32);
        let amp = c.h * r.range(0.04, 0.09);
        let freq = r.range(1.2, 2.6) / c.w * std::f32::consts::TAU;
        let ph = r.range(0.0, 6.0);
        c.hill(base, amp, freq, ph, p.tint(*t));
    }
    c.rect(0.0, c.h * 0.86, c.w, c.h, p.shade(0.25), 1.0);
}

fn still(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.vgradient(p.tint(0.9), p.tint(0.82));
    let floor = c.h * 0.66;
    c.rect(0.0, floor, c.w, c.h, p.tint(0.7), 1.0);
    let n = 2 + r.below(2);
    let mut x = c.w * r.range(0.22, 0.3);
    for i in 0..n {
        let w = c.w * r.range(0.1, 0.16);
        let h = c.h * r.range(0.22, 0.42);
        let col = if i % 2 == 0 {
            p.base()
        } else {
            p.accent_tint(0.25)
        };
        c.ellipse(
            x + w / 2.0,
            floor + 6.0,
            w * 0.75,
            14.0,
            p.shade(0.55),
            0.25,
        );
        if r.below(3) == 0 {
            c.circle(x + w / 2.0, floor - w / 2.0, w / 2.0, col, 1.0);
        } else {
            c.rrect(x, floor - h, x + w, floor, w * 0.22, col, 1.0);
            c.rrect(
                x + w * 0.3,
                floor - h - h * 0.12,
                x + w * 0.7,
                floor - h + 4.0,
                6.0,
                p.shade(0.3),
                1.0,
            );
        }
        x += w + c.w * r.range(0.05, 0.1);
    }
}

fn room(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.rect(0.0, 0.0, c.w, c.h, p.tint(0.88), 1.0);
    let floor = c.h * 0.74;
    c.rect(0.0, floor, c.w, c.h, p.tint(0.62), 1.0);
    let wx = c.w * r.range(0.55, 0.65);
    c.rrect(
        wx,
        c.h * 0.12,
        wx + c.w * 0.26,
        c.h * 0.56,
        8.0,
        p.tint(0.97),
        1.0,
    );
    c.rect(
        wx + c.w * 0.128,
        c.h * 0.12,
        wx + c.w * 0.132,
        c.h * 0.56,
        p.tint(0.7),
        1.0,
    );
    c.rect(
        wx,
        c.h * 0.335,
        wx + c.w * 0.26,
        c.h * 0.345,
        p.tint(0.7),
        1.0,
    );
    c.rrect(
        c.w * 0.12,
        c.h * 0.2,
        c.w * 0.3,
        c.h * 0.38,
        4.0,
        p.accent_tint(0.45),
        1.0,
    );
    let sx = c.w * 0.08;
    c.rrect(
        sx,
        c.h * 0.55,
        sx + c.w * 0.42,
        floor + 8.0,
        26.0,
        p.base(),
        1.0,
    );
    c.rrect(
        sx - 12.0,
        c.h * 0.62,
        sx + c.w * 0.44,
        floor + 20.0,
        22.0,
        p.shade(0.2),
        1.0,
    );
    let px = c.w * 0.9;
    c.rrect(
        px - 34.0,
        floor - 70.0,
        px + 34.0,
        floor + 10.0,
        10.0,
        p.shade(0.35),
        1.0,
    );
    for i in 0..5 {
        let a = i as f32 * 1.3 + r.range(0.0, 0.6);
        c.circle(
            px + a.cos() * 40.0,
            floor - 120.0 + a.sin() * 34.0,
            38.0,
            p.accent_shade(0.15),
            0.9,
        );
    }
}

fn city(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.vgradient(p.accent_tint(0.8), p.tint(0.95));
    let ground = c.h * 0.84;
    let mut x = -10.0;
    let mut row = 0;
    while x < c.w {
        let w = r.range(70.0, 140.0);
        let h = c.h * r.range(0.3, 0.68);
        let col = if row % 3 == 0 {
            p.shade(0.15)
        } else if row % 3 == 1 {
            p.base()
        } else {
            p.tint(0.35)
        };
        c.rect(x, ground - h, x + w - 6.0, ground, col, 1.0);
        let mut wy = ground - h + 18.0;
        while wy < ground - 24.0 {
            let mut wx = x + 12.0;
            while wx < x + w - 24.0 {
                if r.below(4) != 0 {
                    c.rect(wx, wy, wx + 10.0, wy + 14.0, p.tint(0.9), 0.85);
                }
                wx += 22.0;
            }
            wy += 28.0;
        }
        x += w;
        row += 1;
    }
    c.rect(0.0, ground, c.w, c.h, p.shade(0.45), 1.0);
}

fn food(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.rect(0.0, 0.0, c.w, c.h, p.tint(0.78), 1.0);
    for i in 0..6 {
        let y = c.h * (i as f32 / 6.0);
        c.rect(0.0, y, c.w, y + 3.0, p.tint(0.7), 0.6);
    }
    let cx = c.w * r.range(0.42, 0.52);
    let cy = c.h * 0.52;
    c.circle(cx + 10.0, cy + 14.0, 230.0, p.shade(0.6), 0.18);
    c.circle(cx, cy, 230.0, [250, 250, 248], 1.0);
    c.circle(cx, cy, 170.0, p.tint(0.9), 1.0);
    for _ in 0..7 {
        let a = r.range(0.0, std::f32::consts::TAU);
        let d = r.range(20.0, 120.0);
        let col = if r.below(2) == 0 {
            p.base()
        } else {
            p.accent_tint(0.2)
        };
        c.circle(
            cx + a.cos() * d,
            cy + a.sin() * d,
            r.range(22.0, 44.0),
            col,
            1.0,
        );
    }
    let kx = c.w * 0.86;
    c.circle(kx, c.h * 0.24, 70.0, [250, 250, 248], 1.0);
    c.circle(kx, c.h * 0.24, 52.0, p.shade(0.45), 1.0);
}

fn desk(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.rect(0.0, 0.0, c.w, c.h, p.tint(0.74), 1.0);
    let x0 = c.w * r.range(0.1, 0.16);
    let y0 = c.h * 0.14;
    c.rrect(
        x0 + 8.0,
        y0 + 10.0,
        x0 + c.w * 0.42 + 8.0,
        c.h * 0.92 + 10.0,
        10.0,
        p.shade(0.6),
        0.2,
    );
    c.rrect(
        x0,
        y0,
        x0 + c.w * 0.42,
        c.h * 0.92,
        10.0,
        [252, 252, 250],
        1.0,
    );
    c.rect(
        x0 + 34.0,
        y0 + 40.0,
        x0 + c.w * 0.3,
        y0 + 58.0,
        p.base(),
        1.0,
    );
    for i in 0..6 {
        let y = y0 + 90.0 + i as f32 * 26.0;
        let w = c.w * r.range(0.24, 0.34);
        c.rect(x0 + 34.0, y, x0 + 34.0 + w, y + 8.0, p.tint(0.6), 1.0);
    }
    let by = c.h * 0.86;
    for i in 0..5 {
        let bx = x0 + 40.0 + i as f32 * 58.0;
        let h = r.range(40.0, 130.0);
        let col = if i == 3 {
            p.accent_tint(0.1)
        } else {
            p.tint(0.35)
        };
        c.rect(bx, by - h, bx + 38.0, by, col, 1.0);
    }
    let lx = c.w * 0.62;
    c.rrect(
        lx,
        c.h * 0.3,
        lx + c.w * 0.3,
        c.h * 0.7,
        12.0,
        p.shade(0.5),
        1.0,
    );
    c.rrect(
        lx + 12.0,
        c.h * 0.3 + 12.0,
        lx + c.w * 0.3 - 12.0,
        c.h * 0.7 - 12.0,
        6.0,
        p.tint(0.85),
        1.0,
    );
    c.rrect(
        lx - 30.0,
        c.h * 0.7,
        lx + c.w * 0.3 + 30.0,
        c.h * 0.74,
        8.0,
        p.shade(0.35),
        1.0,
    );
    c.circle(c.w * 0.9, c.h * 0.86, 46.0, p.accent_tint(0.3), 1.0);
}

fn people(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.vgradient(p.tint(0.92), p.tint(0.8));
    let n = 5 + r.below(3) as usize;
    let gap = c.w / (n as f32 + 0.5);
    for i in 0..n {
        let x = gap * (i as f32 + 0.75) + r.range(-12.0, 12.0);
        let back = i % 2 == 1;
        let s = if back { 0.82 } else { 1.0 };
        let ground = if back { c.h * 0.82 } else { c.h * 1.02 };
        let col = match i % 4 {
            0 => p.base(),
            1 => p.tint(0.4),
            2 => p.accent_tint(0.2),
            _ => p.shade(0.25),
        };
        let bw = 120.0 * s;
        let bh = 250.0 * s;
        c.rrect(
            x - bw / 2.0,
            ground - bh,
            x + bw / 2.0,
            ground + 40.0,
            54.0 * s,
            col,
            1.0,
        );
        c.circle(x, ground - bh - 52.0 * s, 46.0 * s, p.tint(0.55), 1.0);
    }
}

fn abstract_(c: &mut Canvas, p: &Palette, r: &mut Lcg) {
    c.vgradient(p.tint(0.9), p.accent_tint(0.82));
    for i in 0..5 {
        let col = match i % 3 {
            0 => p.base(),
            1 => p.tint(0.45),
            _ => p.accent_tint(0.3),
        };
        if r.below(2) == 0 {
            c.circle(
                r.range(0.1, 0.9) * c.w,
                r.range(0.15, 0.85) * c.h,
                r.range(60.0, 180.0),
                col,
                0.9,
            );
        } else {
            let x = r.range(0.05, 0.7) * c.w;
            let y = r.range(0.1, 0.7) * c.h;
            c.rrect(
                x,
                y,
                x + r.range(120.0, 300.0),
                y + r.range(80.0, 200.0),
                36.0,
                col,
                0.9,
            );
        }
    }
}

// ---- 색 ----

struct Palette {
    rgb: [f32; 3],
    accent: [f32; 3],
}

impl Palette {
    fn from_hex(hex: &str) -> Self {
        let h = hex.trim().trim_start_matches('#');
        let rgb = if h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()) {
            let ch = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f32;
            [ch(0), ch(2), ch(4)]
        } else {
            [47.0, 93.0, 80.0]
        };
        // 보색 쪽으로 조금 돌린 두 번째 색. 한 가지 색만 쓰면 그림이 밋밋하다.
        let accent = [
            (rgb[2] * 0.6 + 120.0 * 0.4).min(255.0),
            (rgb[0] * 0.5 + 150.0 * 0.5).min(255.0),
            (rgb[1] * 0.6 + 90.0 * 0.4).min(255.0),
        ];
        Self { rgb, accent }
    }

    fn base(&self) -> [u8; 3] {
        to_u8(self.rgb)
    }

    /// 흰색 쪽으로 `t` 만큼.
    fn tint(&self, t: f32) -> [u8; 3] {
        to_u8(mix(self.rgb, [255.0; 3], t))
    }

    /// 검은색 쪽으로 `t` 만큼.
    fn shade(&self, t: f32) -> [u8; 3] {
        to_u8(mix(self.rgb, [18.0, 18.0, 22.0], t))
    }

    fn accent_tint(&self, t: f32) -> [u8; 3] {
        to_u8(mix(self.accent, [255.0; 3], t))
    }

    fn accent_shade(&self, t: f32) -> [u8; 3] {
        to_u8(mix(self.accent, [18.0, 18.0, 22.0], t))
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn to_u8(c: [f32; 3]) -> [u8; 3] {
    [
        c[0].clamp(0.0, 255.0) as u8,
        c[1].clamp(0.0, 255.0) as u8,
        c[2].clamp(0.0, 255.0) as u8,
    ]
}

// ---- 그리기 ----

/// 작은 결정적 난수. 같은 씨앗이면 같은 그림.
struct Lcg(u32);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

struct Canvas {
    w: f32,
    h: f32,
    px: Vec<[f32; 3]>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w: w as f32,
            h: h as f32,
            px: vec![[255.0; 3]; (w * h) as usize],
        }
    }

    fn into_image(self) -> RgbImage {
        let (w, h) = (self.w as u32, self.h as u32);
        RgbImage::from_fn(w, h, |x, y| Rgb(to_u8(self.px[(y * w + x) as usize])))
    }

    fn vgradient(&mut self, top: [u8; 3], bottom: [u8; 3]) {
        let (w, h) = (self.w as usize, self.h as usize);
        for y in 0..h {
            let c = mix(f(top), f(bottom), y as f32 / (h - 1).max(1) as f32);
            for x in 0..w {
                self.px[y * w + x] = c;
            }
        }
    }

    /// 덮을 비율(0~1)을 주는 함수로 사각 범위 안을 칠한다.
    fn paint(
        &mut self,
        (x0, y0, x1, y1): (f32, f32, f32, f32),
        color: [u8; 3],
        alpha: f32,
        cover: impl Fn(f32, f32) -> f32,
    ) {
        let (w, h) = (self.w as i64, self.h as i64);
        let xa = (x0.floor() as i64 - 1).clamp(0, w);
        let xb = (x1.ceil() as i64 + 1).clamp(0, w);
        let ya = (y0.floor() as i64 - 1).clamp(0, h);
        let yb = (y1.ceil() as i64 + 1).clamp(0, h);
        let col = f(color);
        for y in ya..yb {
            for x in xa..xb {
                let a = cover(x as f32 + 0.5, y as f32 + 0.5) * alpha;
                if a <= 0.0 {
                    continue;
                }
                let i = (y * w + x) as usize;
                self.px[i] = mix(self.px[i], col, a.min(1.0));
            }
        }
    }

    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 3], alpha: f32) {
        self.rrect(x0, y0, x1, y1, 0.0, color, alpha);
    }

    #[allow(clippy::too_many_arguments)]
    fn rrect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, rad: f32, color: [u8; 3], alpha: f32) {
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
        let rad = rad.min(hx).min(hy).max(0.0);
        self.paint((x0, y0, x1, y1), color, alpha, |x, y| {
            let qx = (x - cx).abs() - hx + rad;
            let qy = (y - cy).abs() - hy + rad;
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            let d = outside + qx.max(qy).min(0.0) - rad;
            (0.5 - d).clamp(0.0, 1.0)
        });
    }

    fn circle(&mut self, cx: f32, cy: f32, rad: f32, color: [u8; 3], alpha: f32) {
        self.ellipse(cx, cy, rad, rad, color, alpha);
    }

    fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, color: [u8; 3], alpha: f32) {
        let k = rx.min(ry).max(1.0);
        self.paint(
            (cx - rx, cy - ry, cx + rx, cy + ry),
            color,
            alpha,
            |x, y| {
                let nx = (x - cx) / rx.max(1.0);
                let ny = (y - cy) / ry.max(1.0);
                let d = ((nx * nx + ny * ny).sqrt() - 1.0) * k;
                (0.5 - d).clamp(0.0, 1.0)
            },
        );
    }

    /// 사인 곡선 아래를 칠한다 (언덕).
    fn hill(&mut self, base: f32, amp: f32, freq: f32, phase: f32, color: [u8; 3]) {
        let w = self.w;
        let h = self.h;
        self.paint((0.0, base - amp - 2.0, w, h), color, 1.0, |x, y| {
            let top = base + amp * (freq * x + phase).sin();
            (y - top + 0.5).clamp(0.0, 1.0)
        });
    }
}

fn f(c: [u8; 3]) -> [f32; 3] {
    [c[0] as f32, c[1] as f32, c[2] as f32]
}

// ---- 아이콘 (ADR-0162) ----

/// 홈 화면 아이콘의 무늬 수. 사이트마다 씨앗으로 하나를 고른다.
const ICON_MARKS: u32 = 4;

/// 주 색 위에 쓸 무늬 색: 흰색과 짙은 회색 가운데 대비가 큰 쪽.
fn icon_ink(primary: &str) -> &'static str {
    let white = crate::design::contrast(primary, "#ffffff").unwrap_or(0.0);
    let dark = crate::design::contrast(primary, "#1c1c1c").unwrap_or(0.0);
    if white >= dark { "#ffffff" } else { "#1c1c1c" }
}

/// 무늬 하나: 512 기준 좌표의 원·둥근 사각형. 바탕은 주 색으로 가득 칠해 둥근 마스크에 잘려도 된다.
/// 모든 무늬는 가운데 반지름 200 안에 든다(마스크 아이콘 안전 영역).
enum Shape {
    /// 중심 x, y, 반지름, 무늬 색이면 true(아니면 바탕 색으로 뚫기)
    Circle(f32, f32, f32, bool),
    /// x0, y0, x1, y1, 모서리 반지름, 무늬 색이면 true
    Round(f32, f32, f32, f32, f32, bool),
}

fn icon_shapes(seed: u32) -> Vec<Shape> {
    use Shape::{Circle, Round};
    match seed % ICON_MARKS {
        // 고리와 점
        0 => vec![
            Circle(256.0, 256.0, 150.0, true),
            Circle(256.0, 256.0, 96.0, false),
            Circle(256.0, 256.0, 40.0, true),
        ],
        // 초승달 (가로 막대 셋은 메뉴 단추처럼 보여 쓰지 않는다)
        1 => vec![
            Circle(248.0, 264.0, 144.0, true),
            Circle(318.0, 204.0, 124.0, false),
        ],
        // 창
        2 => vec![
            Round(128.0, 128.0, 384.0, 384.0, 64.0, true),
            Round(178.0, 178.0, 334.0, 334.0, 26.0, false),
            Circle(256.0, 256.0, 34.0, true),
        ],
        // 해 뜨는 언덕
        _ => vec![
            Circle(256.0, 290.0, 124.0, true),
            Round(120.0, 290.0, 392.0, 420.0, 0.0, false),
            Round(136.0, 318.0, 376.0, 352.0, 17.0, true),
        ],
    }
}

/// 아이콘 PNG (`size`×`size`). 같은 (씨앗, 색, 크기)면 같은 바이트.
pub fn icon(seed: u32, primary: &str, size: u32) -> Result<Vec<u8>> {
    let size = size.clamp(16, 1024);
    let p = Palette::from_hex(primary);
    let ink = Palette::from_hex(icon_ink(primary)).base();
    let mut c = Canvas::new(size, size);
    c.rect(0.0, 0.0, size as f32, size as f32, p.base(), 1.0);
    let k = size as f32 / 512.0;
    for s in icon_shapes(seed) {
        match s {
            Shape::Circle(x, y, r, on) => {
                c.circle(x * k, y * k, r * k, if on { ink } else { p.base() }, 1.0)
            }
            Shape::Round(x0, y0, x1, y1, r, on) => {
                c.rrect(
                    x0 * k,
                    y0 * k,
                    x1 * k,
                    y1 * k,
                    r * k,
                    if on { ink } else { p.base() },
                    1.0,
                );
            }
        }
    }
    let img = c.into_image();
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(img.as_raw(), size, size, ExtendedColorType::Rgb8)
        .map_err(Error::internal)?;
    Ok(out)
}

/// 같은 무늬의 SVG. 색은 `#rrggbb` 로 다시 써서 넣는다(입력 문자열을 그대로 넣지 않는다).
pub fn icon_svg(seed: u32, primary: &str) -> String {
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    let base = hex(Palette::from_hex(primary).base());
    let ink = hex(Palette::from_hex(icon_ink(primary)).base());
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 512 512\"><rect width=\"512\" height=\"512\" rx=\"96\" fill=\"{base}\"/>"
    );
    for s in icon_shapes(seed) {
        match s {
            Shape::Circle(x, y, r, on) => svg.push_str(&format!(
                "<circle cx=\"{x}\" cy=\"{y}\" r=\"{r}\" fill=\"{}\"/>",
                if on { &ink } else { &base }
            )),
            Shape::Round(x0, y0, x1, y1, r, on) => svg.push_str(&format!(
                "<rect x=\"{x0}\" y=\"{y0}\" width=\"{}\" height=\"{}\" rx=\"{r}\" fill=\"{}\"/>",
                x1 - x0,
                y1 - y0,
                if on { &ink } else { &base }
            )),
        }
    }
    svg.push_str("</svg>");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_motif_renders_a_jpeg() {
        for m in MOTIFS {
            let bytes = render(m, 1, "#b4235a").unwrap();
            assert!(bytes.starts_with(&[0xFF, 0xD8]), "{m}");
            let img = image::load_from_memory(&bytes).unwrap();
            assert_eq!((img.width(), img.height()), (WIDTH, HEIGHT));
        }
    }

    #[test]
    fn share_picture_is_open_graph_size() {
        let bytes = share(7, "#2754c5").unwrap();
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (SHARE_WIDTH, SHARE_HEIGHT));
        assert_eq!(bytes, share(7, "#2754c5").unwrap());
    }

    #[test]
    fn icons_are_square_pngs_with_every_mark() {
        for seed in 0..ICON_MARKS {
            let bytes = icon(seed, "#2754c5", 192).unwrap();
            let img = image::load_from_memory(&bytes).unwrap();
            assert_eq!((img.width(), img.height()), (192, 192));
            // 가장자리는 주 색, 가운데 무늬는 흰색 쪽
            let rgb = img.to_rgb8();
            assert_eq!(rgb.get_pixel(2, 2).0, [0x27, 0x54, 0xc5]);
            let svg = icon_svg(seed, "#2754c5");
            assert!(svg.starts_with("<svg") && svg.contains("#ffffff"), "{svg}");
        }
        // 밝은 주 색에는 짙은 무늬
        assert!(icon_svg(0, "#f5e663").contains("#1c1c1c"));
        // 틀린 색 문자열은 그대로 들어가지 않는다
        assert!(!icon_svg(0, "\"><script>").contains("script"));
    }

    #[test]
    fn same_seed_same_picture() {
        assert_eq!(
            render("city", 3, "#2f5d50").unwrap(),
            render("city", 3, "#2f5d50").unwrap()
        );
        assert!(render("x", 1, "zz").is_ok());
    }
}
