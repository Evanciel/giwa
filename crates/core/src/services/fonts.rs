//! 운영자가 올린 서체 (ADR-0048, docs/plans/font-upload.md).
//!
//! 서체 하나(`fonts`)에 굵기·기울임별 파일(`font_files`)이 붙는다. 파일은 다시 인코딩하지 않고 받은 그대로
//! 내보낸다. 대신 첫 바이트로 형식(woff2·woff·ttf·otf)을 판정하고 머리의 길이·표 위치가 파일 안에 있는지만
//! 가볍게 본다. 깊은 검사는 브라우저의 폰트 정화기(OTS)가 한다.
//!
//! Phase 3 에서 디자인 변경과 함께 changeset 으로 감싼다. 지금은 `design.edit` 권한으로 바로 반영한다.

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::design::{FONT_FALLBACKS, is_font_slug, uploaded_font_family, uploaded_font_stack};
use crate::error::{Code, Error, Result};
use crate::model::{Font, FontFile};
use crate::services::design;
use crate::store::{Ctx, Tx};
use crate::urls;

/// 파일 한 개의 최대 크기. 한글 서체는 글자가 많아 그림보다 크게 잡는다.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;
/// 이보다 크면 받기는 하되 「느릴 수 있음」 경고를 낸다.
pub const WARN_BYTES: usize = 3 * 1024 * 1024;
/// 라이선스 전문 최대 크기.
const MAX_LICENSE_TEXT: usize = 64 * 1024;
const MAX_NAME: usize = 40;

/// 고를 수 있는 라이선스. `other` 는 「웹폰트 사용권을 직접 확인함」.
pub const LICENSES: [&str; 5] = ["OFL-1.1", "Apache-2.0", "MIT", "CC0-1.0", "other"];

/// 서체 만들기 입력.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewFont {
    /// 메뉴에 보일 이름 (40자 이내)
    pub name: String,
    /// `ff-{slug}` 와 토큰 값. 비우면 이름에서 만든다(한글이면 `font-1`…)
    #[serde(default)]
    pub slug: Option<String>,
    /// 못 받았을 때 떨어질 계열: sans | serif | mono | cursive (기본 sans)
    #[serde(default)]
    pub fallback: Option<String>,
    /// OFL-1.1 | Apache-2.0 | MIT | CC0-1.0 | other
    pub license: String,
    /// 라이선스 전문. OFL 은 배포할 때 전문을 함께 두라고 한다
    #[serde(default)]
    pub license_text: Option<String>,
    /// 「이 서체를 웹에 올려 내보낼 권리가 있습니다」 확인. CLI 는 `--yes` 가 대신한다
    #[serde(default)]
    pub rights_confirmed: bool,
}

/// 서체 고치기 입력. 비운 칸은 그대로 둔다.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FontPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub fallback: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// 빈 글자면 붙인 전문을 뗀다
    #[serde(default)]
    pub license_text: Option<String>,
}

/// 파일 올리기 입력.
#[derive(Debug, Clone)]
pub struct FontUpload {
    /// 100~900 (기본 400)
    pub weight: i64,
    pub italic: bool,
    pub bytes: Vec<u8>,
}

/// 파일을 올린 결과. 서체와 함께 경고(큰 파일 등)를 돌려준다.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Uploaded {
    pub font: Font,
    pub warnings: Vec<String>,
}

/// 서체를 지운 결과. 글 안 `:font[…]{slug}` 는 지우지 않고 기본 서체로 보인다.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Removed {
    pub font: Font,
    /// 본문에 이 서체를 쓴 글·페이지 수
    pub used_in_posts: i64,
}

pub async fn list(ctx: &Ctx) -> Result<Vec<Font>> {
    ctx.store.list_fonts().await
}

pub async fn get(ctx: &Ctx, slug: &str) -> Result<Font> {
    ctx.store
        .font_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))
}

/// 서체를 만든다. 같은 slug 가 있으면 이름·대체 계열·라이선스를 맞춰 돌려준다(멱등).
pub async fn add(ctx: &Ctx, actor: &Actor, input: NewFont) -> Result<Font> {
    actor.require(Permission::DesignEdit)?;
    if !input.rights_confirmed {
        return Err(Error::new(Code::Validation)
            .with_message("error.font_rights")
            .next("giwa font add <파일> --name <이름> --license OFL-1.1 --yes"));
    }
    let name = clean_name(&input.name)?;
    let fallback = check_fallback(input.fallback.as_deref().unwrap_or("sans"))?;
    let license = check_license(&input.license)?;
    let text = input
        .license_text
        .as_deref()
        .map(check_license_text)
        .transpose()?;

    let mut tx = Tx::begin(ctx).await?;
    let slug = match input.slug.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => check_slug(s)?,
        None => free_slug(ctx, &mut tx, &name).await?,
    };
    if let Some(before) = tx.font_by_slug(&slug).await? {
        drop(tx);
        let patch = FontPatch {
            name: Some(name),
            fallback: Some(fallback),
            license: Some(license),
            license_text: input.license_text,
        };
        return update(ctx, actor, &before.slug, patch).await;
    }
    let now = clock::now();
    let license_path = match text {
        Some(t) if !t.is_empty() => Some(put_license(ctx, &slug, t).await?),
        _ => None,
    };
    let font = Font {
        id: clock::new_id(),
        license_url: license_path.as_ref().map(|_| urls::font_license(&slug)),
        slug,
        name,
        fallback,
        license,
        license_path: license_path.clone(),
        files: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    let saved = async {
        tx.insert_font(&font).await?;
        tx.audit(
            actor,
            "font.add",
            Target::new("font", &font.slug),
            audit::diff(None, Some(&font)),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = saved {
        if let Some(p) = &license_path {
            let _ = ctx.blobs.delete(p).await;
        }
        return Err(e);
    }
    Ok(font)
}

/// 서체에 파일을 붙인다. 같은 굵기·기울임이 있으면 바꾸고, 내용이 같으면 아무것도 쓰지 않는다.
pub async fn add_file(ctx: &Ctx, actor: &Actor, slug: &str, input: FontUpload) -> Result<Uploaded> {
    actor.require(Permission::DesignEdit)?;
    check_weight(input.weight)?;
    let format = sniff(&input.bytes)?;
    let sha256 = hex::encode(Sha256::digest(&input.bytes));
    let mut warnings = Vec::new();
    if input.bytes.len() > WARN_BYTES {
        warnings.push(crate::messages::render(
            "warn.font_large",
            &[("size", mb(input.bytes.len()))].into_iter().collect(),
        ));
    }

    let mut tx = Tx::begin(ctx).await?;
    let font = tx
        .font_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    let before = font
        .files
        .iter()
        .find(|f| f.weight == input.weight && f.italic == input.italic)
        .cloned();
    if before.as_ref().is_some_and(|b| b.sha256 == sha256) {
        drop(tx);
        return Ok(Uploaded { font, warnings });
    }
    let path = format!(
        "fonts/{slug}/{}{}-{}.{format}",
        input.weight,
        if input.italic { "i" } else { "" },
        &sha256[..16]
    );
    ctx.blobs.put(&path, &input.bytes).await?;
    let file = FontFile {
        id: clock::new_id(),
        weight: input.weight,
        italic: input.italic,
        format: format.to_owned(),
        url: urls::font_file(slug, &path),
        path: path.clone(),
        size: i64::try_from(input.bytes.len()).unwrap_or(i64::MAX),
        sha256,
        created_at: clock::now(),
    };
    let saved = async {
        if let Some(b) = &before {
            tx.delete_font_file(&b.id).await?;
        }
        tx.insert_font_file(&font.id, &file).await?;
        tx.audit(
            actor,
            "font.file.add",
            Target::new("font", slug),
            audit::diff(before.as_ref(), Some(&file)),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = saved {
        let _ = ctx.blobs.delete(&path).await;
        return Err(e);
    }
    if let Some(b) = before.filter(|b| b.path != path) {
        let _ = ctx.blobs.delete(&b.path).await;
    }
    Ok(Uploaded {
        font: get(ctx, slug).await?,
        warnings,
    })
}

/// 서체를 만들고(있으면 맞추고) 파일 하나를 붙인다. CLI `giwa font add` 와 관리 화면 올리기가 쓴다.
/// 파일을 먼저 검사해서 깨진 파일로 빈 서체가 생기지 않게 하고, 그래도 실패하면 이번에 만든 서체를 지운다.
pub async fn add_with_file(
    ctx: &Ctx,
    actor: &Actor,
    input: NewFont,
    upload: FontUpload,
) -> Result<Uploaded> {
    actor.require(Permission::DesignEdit)?;
    check_weight(upload.weight)?;
    sniff(&upload.bytes)?;
    let before: Vec<String> = list(ctx).await?.into_iter().map(|f| f.id).collect();
    let font = add(ctx, actor, input).await?;
    match add_file(ctx, actor, &font.slug, upload).await {
        Ok(up) => Ok(up),
        Err(e) => {
            if !before.contains(&font.id) {
                let _ = remove(ctx, actor, &font.slug).await;
            }
            Err(e)
        }
    }
}

pub async fn update(ctx: &Ctx, actor: &Actor, slug: &str, patch: FontPatch) -> Result<Font> {
    actor.require(Permission::DesignEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx
        .font_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    let mut after = before.clone();
    if let Some(n) = &patch.name {
        after.name = clean_name(n)?;
    }
    if let Some(f) = &patch.fallback {
        after.fallback = check_fallback(f)?;
    }
    if let Some(l) = &patch.license {
        after.license = check_license(l)?;
    }
    // 전문은 내용 해시 경로라 같은 글이면 같은 경로다.
    let mut new_blob = None;
    if let Some(t) = patch.license_text.as_deref() {
        let t = check_license_text(t)?;
        after.license_path = if t.is_empty() {
            None
        } else {
            let p = license_blob_path(slug, t);
            if before.license_path.as_deref() != Some(p.as_str()) {
                new_blob = Some(put_license(ctx, slug, t).await?);
            }
            Some(p)
        };
        after.license_url = after
            .license_path
            .as_ref()
            .map(|_| urls::font_license(slug));
    }
    if after == before {
        return Ok(before);
    }
    after.updated_at = clock::now();
    let saved = async {
        tx.update_font(&after).await?;
        tx.audit(
            actor,
            "font.update",
            Target::new("font", slug),
            audit::diff(Some(&before), Some(&after)),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = saved {
        if let Some(p) = &new_blob {
            let _ = ctx.blobs.delete(p).await;
        }
        return Err(e);
    }
    if let Some(old) = before
        .license_path
        .filter(|p| after.license_path.as_ref() != Some(p))
    {
        let _ = ctx.blobs.delete(&old).await;
    }
    Ok(after)
}

/// 굵기·기울임 하나의 파일을 뗀다.
pub async fn remove_file(
    ctx: &Ctx,
    actor: &Actor,
    slug: &str,
    weight: i64,
    italic: bool,
) -> Result<Font> {
    actor.require(Permission::DesignEdit)?;
    let mut tx = Tx::begin(ctx).await?;
    let font = tx
        .font_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    let Some(file) = font
        .files
        .iter()
        .find(|f| f.weight == weight && f.italic == italic)
        .cloned()
    else {
        return Err(Error::new(Code::NotFound)
            .with_message("error.font_file_not_found")
            .arg("slug", slug)
            .arg("weight", weight.to_string())
            .next("giwa font list --json"));
    };
    tx.delete_font_file(&file.id).await?;
    tx.audit(
        actor,
        "font.file.remove",
        Target::new("font", slug),
        audit::diff(Some(&file), None),
    )
    .await?;
    tx.commit().await?;
    let _ = ctx.blobs.delete(&file.path).await;
    get(ctx, slug).await
}

/// 서체를 지운다. 디자인 토큰이 쓰고 있으면 거절한다. 글 안 꾸밈은 막지 않는다(기본 서체로 보인다).
pub async fn remove(ctx: &Ctx, actor: &Actor, slug: &str) -> Result<Removed> {
    actor.require(Permission::DesignEdit)?;
    let tokens = design::tokens(ctx).await?;
    for (field, value) in [
        ("font.body", &tokens.font.body),
        ("font.heading", &tokens.font.heading),
        ("font.mono", &tokens.font.mono),
    ] {
        if value == slug {
            return Err(Error::new(Code::Conflict)
                .with_message("error.font_in_use")
                .arg("slug", slug)
                .arg("field", field)
                .next(format!(
                    "giwa design set --set {field}={}",
                    if field == "font.mono" {
                        "D2Coding"
                    } else {
                        "Pretendard"
                    }
                )));
        }
    }
    let mut tx = Tx::begin(ctx).await?;
    let font = tx
        .font_by_slug(slug)
        .await?
        .ok_or_else(|| not_found(slug))?;
    tx.delete_font(&font.id).await?;
    tx.audit(
        actor,
        "font.remove",
        Target::new("font", slug),
        audit::diff(Some(&font), None),
    )
    .await?;
    tx.commit().await?;
    for f in &font.files {
        let _ = ctx.blobs.delete(&f.path).await;
    }
    if let Some(p) = &font.license_path {
        let _ = ctx.blobs.delete(p).await;
    }
    let used_in_posts = ctx.store.count_font_uses(slug).await?;
    Ok(Removed {
        font,
        used_in_posts,
    })
}

/// 공개 파일 내려받기 (`/fonts/{slug}/{name}`). DB 에 있는 경로만 낸다. (형식, 바이트)
pub async fn file(ctx: &Ctx, slug: &str, name: &str) -> Result<(String, Vec<u8>)> {
    if !is_font_slug(slug) {
        return Err(not_found(slug));
    }
    let font = get(ctx, slug).await?;
    let path = if name == "LICENSE.txt" {
        font.license_path.clone()
    } else {
        font.files
            .iter()
            .find(|f| f.path.rsplit('/').next() == Some(name))
            .map(|f| f.path.clone())
    };
    let path = path.ok_or_else(|| not_found(slug))?;
    let format = if name == "LICENSE.txt" {
        "txt".to_owned()
    } else {
        font.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.format.clone())
            .unwrap_or_default()
    };
    let bytes = ctx.blobs.get(&path).await?.ok_or_else(|| not_found(slug))?;
    Ok((format, bytes))
}

/// 형식에 맞는 응답 MIME.
pub fn mime(format: &str) -> &'static str {
    match format {
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "text/plain; charset=utf-8",
    }
}

/// `@font-face` 와 `.ff-{slug}` 규칙. slug·경로는 정해진 모양이라 CSS 에 그대로 넣어도 안전하다.
pub fn css(fonts: &[Font]) -> String {
    let mut css = String::from("/* Giwa 올린 서체. 직접 고치지 말고 `giwa font` 를 쓴다. */\n");
    for font in fonts {
        let family = uploaded_font_family(&font.slug);
        for f in &font.files {
            let format = match f.format.as_str() {
                "ttf" => "truetype",
                "otf" => "opentype",
                other => other,
            };
            let _ = writeln!(
                css,
                "@font-face {{ font-family: \"{family}\"; src: url(\"{}\") format(\"{format}\"); \
                 font-weight: {}; font-style: {}; font-display: swap; }}",
                f.url,
                f.weight,
                if f.italic { "italic" } else { "normal" }
            );
        }
        let _ = writeln!(
            css,
            ".ff-{} {{ font-family: {}; }}",
            font.slug,
            uploaded_font_stack(&font.slug, &font.fallback)
        );
    }
    css
}

/// 첫 바이트로 형식을 판정하고 머리의 길이·표 위치가 파일 안에 있는지 본다.
fn sniff(bytes: &[u8]) -> Result<&'static str> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(Code::Validation)
            .with_message("error.media_too_large")
            .arg("max", (MAX_BYTES / 1024 / 1024).to_string()));
    }
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let format = match bytes.get(..4) {
        Some(b"wOF2") => "woff2",
        Some(b"wOFF") => "woff",
        Some([0, 1, 0, 0] | b"true") => "ttf",
        Some(b"OTTO") => "otf",
        _ => return Err(bad_font()),
    };
    let ok = match format {
        // WOFF·WOFF2 머리: 서명, flavor, 전체 길이, 표 개수
        "woff" | "woff2" => u32_at(8) == Some(bytes.len()) && u16_at(12).is_some_and(|n| n > 0),
        // sfnt: 표 개수 다음에 16바이트짜리 표 목록, 각 표의 (위치, 길이)는 파일 안
        _ => u16_at(4).is_some_and(|n| {
            n > 0
                && (0..usize::from(n)).all(|t| {
                    let rec = 12 + t * 16;
                    match (u32_at(rec + 8), u32_at(rec + 12)) {
                        (Some(off), Some(len)) => {
                            off.checked_add(len).is_some_and(|end| end <= bytes.len())
                        }
                        _ => false,
                    }
                })
        }),
    };
    if ok { Ok(format) } else { Err(bad_font()) }
}

fn check_weight(weight: i64) -> Result<()> {
    if (100..=900).contains(&weight) && weight % 100 == 0 {
        Ok(())
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.font_weight")
            .arg("value", weight.to_string()))
    }
}

fn bad_font() -> Error {
    Error::new(Code::Validation)
        .with_message("error.font_file")
        .arg("allowed", "woff2, woff, ttf, otf")
}

fn clean_name(raw: &str) -> Result<String> {
    let name: String = raw.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim().to_owned();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(Error::new(Code::Validation)
            .with_message("error.font_name")
            .arg("max", MAX_NAME.to_string()));
    }
    Ok(name)
}

fn check_slug(s: &str) -> Result<String> {
    if is_font_slug(s) {
        Ok(s.to_owned())
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.font_slug")
            .arg("value", s.chars().take(40).collect::<String>()))
    }
}

fn check_fallback(s: &str) -> Result<String> {
    if FONT_FALLBACKS.iter().any(|(n, _)| *n == s) {
        Ok(s.to_owned())
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.font_fallback")
            .arg("value", s.chars().take(40).collect::<String>())
            .arg(
                "allowed",
                FONT_FALLBACKS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", "),
            ))
    }
}

fn check_license(s: &str) -> Result<String> {
    if LICENSES.contains(&s) {
        Ok(s.to_owned())
    } else {
        Err(Error::new(Code::Validation)
            .with_message("error.font_license")
            .arg("value", s.chars().take(40).collect::<String>())
            .arg("allowed", LICENSES.join(", "))
            .next("giwa font add <파일> --name <이름> --license OFL-1.1 --yes"))
    }
}

fn check_license_text(t: &str) -> Result<&str> {
    if t.len() > MAX_LICENSE_TEXT {
        return Err(Error::new(Code::Validation)
            .with_message("error.font_license_text")
            .arg("max", (MAX_LICENSE_TEXT / 1024).to_string()));
    }
    Ok(t.trim())
}

fn license_blob_path(slug: &str, text: &str) -> String {
    let sha = hex::encode(Sha256::digest(text.as_bytes()));
    format!("fonts/{slug}/license-{}.txt", &sha[..16])
}

async fn put_license(ctx: &Ctx, slug: &str, text: &str) -> Result<String> {
    let path = license_blob_path(slug, text);
    ctx.blobs.put(&path, text.as_bytes()).await?;
    Ok(path)
}

/// slug 를 주지 않았을 때. 같은 이름의 서체가 있으면 그 slug(멱등), 영문 이름이면 이름에서 만든 slug,
/// 그 밖에는 비어 있는 `font-1`, `font-2` …
async fn free_slug(ctx: &Ctx, tx: &mut Tx, name: &str) -> Result<String> {
    if let Some(same) = ctx
        .store
        .list_fonts()
        .await?
        .into_iter()
        .find(|f| f.name == name)
    {
        return Ok(same.slug);
    }
    let mut base = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('-') && !base.is_empty() {
            base.push('-');
        }
    }
    let base = base
        .trim_end_matches('-')
        .chars()
        .take(32)
        .collect::<String>();
    let base = base.trim_end_matches('-').to_owned();
    if is_font_slug(&base) {
        return Ok(base);
    }
    for n in 1..1000 {
        let s = format!("font-{n}");
        if tx.font_by_slug(&s).await?.is_none() {
            return Ok(s);
        }
    }
    Err(Error::internal("서체 slug 를 만들지 못했다"))
}

fn mb(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / 1024.0 / 1024.0)
}

fn not_found(slug: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.font_not_found")
        .arg("slug", slug.chars().take(40).collect::<String>())
        .next("giwa font list")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 표 하나짜리 최소 sfnt.
    fn tiny_ttf() -> Vec<u8> {
        let mut b = vec![0, 1, 0, 0, 0, 1, 0, 16, 0, 0, 0, 0];
        b.extend_from_slice(b"head");
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&28u32.to_be_bytes());
        b.extend_from_slice(&4u32.to_be_bytes());
        b.extend_from_slice(&[1, 2, 3, 4]);
        b
    }

    fn tiny_woff2() -> Vec<u8> {
        let mut b = b"wOF2".to_vec();
        b.extend_from_slice(&[0, 1, 0, 0]);
        b.extend_from_slice(&16u32.to_be_bytes());
        b.extend_from_slice(&[0, 1, 0, 0]);
        b
    }

    #[test]
    fn sniff_reads_signature_and_bounds() {
        assert_eq!(sniff(&tiny_ttf()).unwrap(), "ttf");
        assert_eq!(sniff(&tiny_woff2()).unwrap(), "woff2");
        let mut otf = tiny_ttf();
        otf[..4].copy_from_slice(b"OTTO");
        assert_eq!(sniff(&otf).unwrap(), "otf");
        // 표가 파일 밖을 가리킴, 길이가 맞지 않는 woff2, 글꼴이 아닌 것
        let mut broken = tiny_ttf();
        broken.truncate(30);
        let mut short = tiny_woff2();
        short.push(0);
        for bad in [
            broken,
            short,
            b"<svg onload=alert(1)>".to_vec(),
            b"PK\x03\x04".to_vec(),
            vec![],
        ] {
            assert_eq!(sniff(&bad).unwrap_err().code(), Code::Validation);
        }
    }

    #[test]
    fn css_uses_family_prefix_and_formats() {
        let font = Font {
            id: "1".into(),
            slug: "company-gothic".into(),
            name: "회사 고딕".into(),
            fallback: "sans".into(),
            license: "OFL-1.1".into(),
            license_path: None,
            license_url: None,
            files: vec![FontFile {
                id: "2".into(),
                weight: 700,
                italic: false,
                format: "ttf".into(),
                path: "fonts/company-gothic/700-abc.ttf".into(),
                url: "/fonts/company-gothic/700-abc.ttf".into(),
                size: 1,
                sha256: "abc".into(),
                created_at: String::new(),
            }],
            created_at: String::new(),
            updated_at: String::new(),
        };
        let css = css(&[font]);
        assert!(css.contains(
            "font-family: \"maru-u-company-gothic\"; src: url(\"/fonts/company-gothic/700-abc.ttf\") format(\"truetype\"); font-weight: 700"
        ));
        assert!(css.contains(".ff-company-gothic { font-family: \"maru-u-company-gothic\", "));
    }
}
