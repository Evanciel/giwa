//! 미디어 업로드 (SPEC §13 업로드 규칙, ADR-0032).
//!
//! 내용 스니핑과 확장자 화이트리스트를 둘 다 통과해야 하고, 원본은 그대로 두지 않고 다시 인코딩한다.
//! 다시 인코딩하면 EXIF(촬영 위치 등)와 이미지 뒤에 덧붙인 데이터가 사라진다.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{Listing, Media, MediaVariant, PageReq};
use crate::store::{Ctx, Tx};
use crate::urls;

/// 업로드 한 건의 최대 크기.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// 가로·세로 최대 픽셀. 압축 폭탄을 디코딩 전에 막는다.
const MAX_SIDE: u32 = 8192;
/// 디코딩에 쓸 수 있는 최대 메모리.
const MAX_ALLOC: u64 = 300 * 1024 * 1024;
/// 만들어 두는 축소본 폭. 원본이 이보다 넓을 때만 만든다.
pub const VARIANT_WIDTHS: [u32; 2] = [640, 1280];
const JPEG_QUALITY: u8 = 85;
const ALT_MAX: usize = 300;
const EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "gif", "webp"];

/// 업로드 입력. 파일 이름은 확장자 검사에만 쓰고 저장 경로에는 넣지 않는다.
#[derive(Debug, Clone)]
pub struct Upload {
    pub filename: String,
    pub bytes: Vec<u8>,
    pub alt_text: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaPatch {
    /// 빈 문자열이면 지운다.
    pub alt_text: Option<String>,
}

/// 저장 형식. GIF 는 첫 장만 PNG 로, 투명도 없는 WebP 는 JPEG 로 바꾼다 (ADR-0032).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Out {
    Jpeg,
    Png,
}

impl Out {
    fn ext(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

struct Encoded {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

struct Processed {
    out: Out,
    original: Encoded,
    variants: Vec<Encoded>,
}

pub async fn upload(ctx: &Ctx, actor: &Actor, input: Upload) -> Result<Media> {
    actor.require(Permission::PostUpload)?;
    let alt_text = alt(input.alt_text)?;
    let p = process(&input.filename, &input.bytes)?;

    let id = clock::new_id();
    let now = clock::now();
    // 시각 형식은 `YYYY-MM-DDT…` 고정 자릿수다 (clock).
    let dir = format!("{}/{}", &now[0..4], &now[5..7]);
    let path = format!("{dir}/{id}.{}", p.out.ext());
    let mut written = Vec::new();
    let result = async {
        ctx.blobs.put(&path, &p.original.bytes).await?;
        written.push(path.clone());
        let mut variants = Vec::new();
        for v in &p.variants {
            let vpath = format!("{dir}/{id}-{}.{}", v.width, p.out.ext());
            ctx.blobs.put(&vpath, &v.bytes).await?;
            written.push(vpath.clone());
            variants.push(MediaVariant {
                width: v.width,
                height: v.height,
                url: urls::media(&vpath),
                path: vpath,
                size: len(&v.bytes),
            });
        }
        let media = Media {
            id: id.clone(),
            url: urls::media(&path),
            path: path.clone(),
            mime: p.out.mime().to_owned(),
            size: len(&p.original.bytes),
            width: p.original.width,
            height: p.original.height,
            alt_text,
            variants,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        let mut tx = Tx::begin(ctx).await?;
        tx.insert_media(&media).await?;
        tx.audit(
            actor,
            "media.upload",
            Target::new("media", &media.id),
            audit::diff(None, Some(&media)),
        )
        .await?;
        tx.commit().await?;
        Ok(media)
    }
    .await;
    if result.is_err() {
        // DB 에 남지 않은 파일은 지운다. 지우기 실패는 원래 오류를 가리지 않게 무시한다.
        for p in &written {
            let _ = ctx.blobs.delete(p).await;
        }
    }
    result
}

pub async fn list(ctx: &Ctx, actor: &Actor, page: PageReq) -> Result<Listing<Media>> {
    actor.require(Permission::PostUpload)?;
    let total = ctx.store.count_media().await?;
    let items = ctx.store.list_media(page.limit(), page.offset()).await?;
    Ok(Listing {
        items,
        total,
        page: page.page,
        per_page: page.per_page,
    })
}

pub async fn get(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Media> {
    actor.require(Permission::PostUpload)?;
    ctx.store
        .media_by_id(id)
        .await?
        .ok_or_else(|| not_found(id))
}

/// 대체 텍스트를 바꾼다. 같은 값이면 쓰지 않는다 (멱등).
pub async fn update(ctx: &Ctx, actor: &Actor, id: &str, patch: MediaPatch) -> Result<Media> {
    actor.require(Permission::PostUpload)?;
    let alt_text = alt(patch.alt_text)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx.media_by_id(id).await?.ok_or_else(|| not_found(id))?;
    if before.alt_text == alt_text {
        return Ok(before);
    }
    let after = Media {
        alt_text,
        updated_at: clock::now(),
        ..before.clone()
    };
    tx.update_media(&after).await?;
    tx.audit(
        actor,
        "media.update",
        Target::new("media", id),
        audit::diff(Some(&before), Some(&after)),
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// 기록과 파일을 지운다. 본문에서 이 이미지를 쓰던 곳은 깨진다.
pub async fn delete(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Media> {
    actor.require(Permission::PostModerate)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx.media_by_id(id).await?.ok_or_else(|| not_found(id))?;
    tx.delete_media(id).await?;
    tx.audit(
        actor,
        "media.delete",
        Target::new("media", id),
        audit::diff(Some(&before), None),
    )
    .await?;
    tx.commit().await?;
    // 기록이 지워진 뒤에 파일을 지운다. 실패하면 고아 파일만 남는다.
    for p in std::iter::once(&before.path).chain(before.variants.iter().map(|v| &v.path)) {
        let _ = ctx.blobs.delete(p).await;
    }
    Ok(before)
}

fn not_found(id: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.media_not_found")
        .arg("id", id.to_owned())
        .next("maru media list")
}

fn alt(value: Option<String>) -> Result<Option<String>> {
    let Some(v) = value else { return Ok(None) };
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    if v.chars().count() > ALT_MAX {
        return Err(Error::new(Code::Validation)
            .with_message("error.field_length")
            .arg("field", "alt_text")
            .arg("max", ALT_MAX.to_string()));
    }
    Ok(Some(v.to_owned()))
}

fn len(bytes: &[u8]) -> i64 {
    i64::try_from(bytes.len()).unwrap_or(i64::MAX)
}

fn unsupported(filename: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.media_type")
        .arg("name", filename.to_owned())
}

fn undecodable(e: image::ImageError) -> Error {
    Error::new(Code::Validation)
        .with_message("error.media_decode")
        .arg("detail", e.to_string())
}

/// 검사 → 디코딩 → 방향 보정 → 다시 인코딩 → 축소본. CPU 를 쓰는 동기 함수다.
fn process(filename: &str, bytes: &[u8]) -> Result<Processed> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(Code::Validation)
            .with_message("error.media_too_large")
            .arg("max", (MAX_BYTES / 1024 / 1024).to_string()));
    }
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err(unsupported(filename));
    }
    let format = image::guess_format(bytes).map_err(|_| unsupported(filename))?;
    if !matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return Err(unsupported(filename));
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(undecodable)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(undecodable)?;
    img.apply_orientation(orientation);

    let out = match format {
        ImageFormat::Jpeg => Out::Jpeg,
        ImageFormat::WebP if !img.color().has_alpha() => Out::Jpeg,
        _ => Out::Png,
    };
    let original = encode(&img, out)?;
    let mut variants = Vec::new();
    for w in VARIANT_WIDTHS {
        if img.width() > w {
            let h = scaled_height(img.width(), img.height(), w);
            let small = img.resize_exact(w, h, FilterType::CatmullRom);
            variants.push(encode(&small, out)?);
        }
    }
    Ok(Processed {
        out,
        original,
        variants,
    })
}

fn scaled_height(width: u32, height: u32, to: u32) -> u32 {
    let h = u64::from(height) * u64::from(to) / u64::from(width.max(1));
    u32::try_from(h).unwrap_or(u32::MAX).max(1)
}

fn encode(img: &DynamicImage, out: Out) -> Result<Encoded> {
    let mut bytes = Vec::new();
    match out {
        Out::Jpeg => {
            let rgb = img.to_rgb8();
            JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
                .encode_image(&rgb)
                .map_err(Error::internal)?;
        }
        Out::Png => {
            img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
                .map_err(Error::internal)?;
        }
    }
    Ok(Encoded {
        width: img.width(),
        height: img.height(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([200, 100, 50])));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    fn png_alpha(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 10])));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn jpeg_gets_variants_narrower_than_original() {
        let p = process("사진.JPG", &jpeg(1500, 1000)).unwrap();
        assert_eq!(p.out, Out::Jpeg);
        assert_eq!((p.original.width, p.original.height), (1500, 1000));
        let sizes: Vec<_> = p.variants.iter().map(|v| (v.width, v.height)).collect();
        assert_eq!(sizes, vec![(640, 426), (1280, 853)]);
        assert_eq!(
            image::guess_format(&p.original.bytes).unwrap(),
            ImageFormat::Jpeg
        );
    }

    #[test]
    fn small_image_has_no_variants() {
        let p = process("a.png", &png_alpha(300, 200)).unwrap();
        assert_eq!(p.out, Out::Png);
        assert!(p.variants.is_empty());
    }

    #[test]
    fn extension_and_content_must_both_be_images() {
        let png = png_alpha(10, 10);
        for name in ["a.svg", "a.html", "a", "a.jpg.exe"] {
            let e = process(name, &png).err().unwrap();
            assert_eq!(e.code(), Code::Validation, "{name}");
        }
        // 확장자는 그림인데 내용은 HTML·SVG
        for body in [
            &b"<html><script>alert(1)</script>"[..],
            b"<svg xmlns='http://www.w3.org/2000/svg'/>",
        ] {
            let e = process("a.png", body).err().unwrap();
            assert_eq!(e.message_key(), "error.media_type");
        }
    }

    #[test]
    fn oversize_and_truncated_files_are_rejected() {
        let big = vec![0u8; MAX_BYTES + 1];
        assert_eq!(
            process("a.jpg", &big).err().unwrap().message_key(),
            "error.media_too_large"
        );
        // 뒤만 잘린 JPEG 는 디코더가 회색으로 채워 읽는다. 머리만 남은 파일은 읽을 수 없다.
        let mut cut = jpeg(400, 400);
        cut.truncate(24);
        assert_eq!(
            process("a.jpg", &cut).err().unwrap().message_key(),
            "error.media_decode"
        );
    }

    #[test]
    fn huge_dimensions_are_rejected_before_decoding() {
        // 9000x10 PNG: 파일은 작지만 한 변이 상한을 넘는다.
        let img = DynamicImage::ImageRgb8(RgbImage::new(9000, 10));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        assert_eq!(
            process("a.png", &buf).err().unwrap().message_key(),
            "error.media_decode"
        );
    }

    #[test]
    fn trailing_payload_is_dropped_by_reencoding() {
        let mut bytes = jpeg(100, 100);
        bytes.extend_from_slice(b"<script>alert(1)</script>");
        let p = process("a.jpg", &bytes).unwrap();
        let tail = b"<script>";
        assert!(!p.original.bytes.windows(tail.len()).any(|w| w == tail));
    }

    #[test]
    fn alt_text_is_trimmed_and_bounded() {
        assert_eq!(alt(Some("  ".into())).unwrap(), None);
        assert_eq!(
            alt(Some(" 고양이 ".into())).unwrap().as_deref(),
            Some("고양이")
        );
        assert!(alt(Some("가".repeat(ALT_MAX + 1))).is_err());
    }
}
