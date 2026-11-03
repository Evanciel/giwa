//! 첨부 파일 (PDF·문서·압축 등).
//!
//! 그림(`media`)과 달리 다시 인코딩하지 않는다. 대신 확장자 화이트리스트와 내용 첫 바이트(서명)를
//! 둘 다 통과해야 하고, 내보낼 때는 항상 내려받기(`attachment`)로, 정해진 MIME 으로만 보낸다.
//! 그래서 HTML·SVG·스크립트를 문서로 꾸며 올려도 사이트 안에서 열리지 않는다.

use crate::actor::{Actor, Permission};
use crate::audit::{self, Target};
use crate::clock;
use crate::error::{Code, Error, Result};
use crate::model::{Attachment, Listing, PageReq};
use crate::services::media;
use crate::store::{Ctx, Tx};
use crate::urls;

/// 한 건의 최대 크기. 요청 본문 상한을 그림과 같이 쓴다.
pub const MAX_BYTES: usize = media::MAX_BYTES;
const MAX_NAME: usize = 120;

/// 내용 서명 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sniff {
    Pdf,
    /// ZIP 컨테이너 (zip, docx, xlsx, pptx, hwpx)
    Zip,
    /// OLE 복합 문서 (hwp, doc, xls, ppt)
    Ole,
    /// UTF-8 글자 (txt, csv, md)
    Text,
}

/// (확장자, 내보낼 MIME, 서명)
const KINDS: [(&str, &str, Sniff); 14] = [
    ("pdf", "application/pdf", Sniff::Pdf),
    ("zip", "application/zip", Sniff::Zip),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Sniff::Zip,
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Sniff::Zip,
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Sniff::Zip,
    ),
    ("hwpx", "application/vnd.hancom.hwpx", Sniff::Zip),
    ("hwp", "application/x-hwp", Sniff::Ole),
    ("doc", "application/msword", Sniff::Ole),
    ("xls", "application/vnd.ms-excel", Sniff::Ole),
    ("ppt", "application/vnd.ms-powerpoint", Sniff::Ole),
    ("txt", "text/plain; charset=utf-8", Sniff::Text),
    ("csv", "text/csv; charset=utf-8", Sniff::Text),
    ("md", "text/markdown; charset=utf-8", Sniff::Text),
    ("markdown", "text/markdown; charset=utf-8", Sniff::Text),
];

/// 받을 수 있는 확장자 (에러 메시지·안내용).
pub fn allowed() -> String {
    KINDS.map(|k| k.0).join(", ")
}

/// 업로드 입력. 파일 이름은 확장자 판단과 내려받기 이름에만 쓰고 보관 경로에는 넣지 않는다.
#[derive(Debug, Clone)]
pub struct Upload {
    pub filename: String,
    pub bytes: Vec<u8>,
}

pub async fn upload(ctx: &Ctx, actor: &Actor, input: Upload) -> Result<Attachment> {
    actor.require(Permission::PostUpload)?;
    let filename = clean_name(&input.filename)?;
    let (ext, mime) = check(&filename, &input.bytes)?;

    let id = clock::new_id();
    let now = clock::now();
    // 시각 형식은 `YYYY-MM-DDT…` 고정 자릿수다 (clock).
    let path = format!("files/{}/{}/{id}.{ext}", &now[0..4], &now[5..7]);
    ctx.blobs.put(&path, &input.bytes).await?;
    let file = Attachment {
        url: urls::file(&id),
        id,
        path: path.clone(),
        filename,
        mime: mime.to_owned(),
        size: i64::try_from(input.bytes.len()).unwrap_or(i64::MAX),
        created_at: now,
    };
    let saved = async {
        let mut tx = Tx::begin(ctx).await?;
        tx.insert_attachment(&file).await?;
        tx.audit(
            actor,
            "file.upload",
            Target::new("file", &file.id),
            audit::diff(None, Some(&file)),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = saved {
        // DB 에 남지 않은 파일은 지운다. 지우기 실패는 원래 오류를 가리지 않게 무시한다.
        let _ = ctx.blobs.delete(&path).await;
        return Err(e);
    }
    Ok(file)
}

pub async fn get(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Attachment> {
    actor.require(Permission::PostUpload)?;
    ctx.store
        .attachment_by_id(id)
        .await?
        .ok_or_else(|| not_found(id))
}

pub async fn list(ctx: &Ctx, actor: &Actor, page: PageReq) -> Result<Listing<Attachment>> {
    actor.require(Permission::PostUpload)?;
    let total = ctx.store.count_attachments().await?;
    let items = ctx
        .store
        .list_attachments(page.limit(), page.offset())
        .await?;
    Ok(Listing {
        items,
        total,
        page: page.page,
        per_page: page.per_page,
    })
}

/// 공개 내려받기. 글에 링크로 붙이는 파일이라 그림(`/media/*`)처럼 권한을 보지 않는다.
pub async fn download(ctx: &Ctx, id: &str) -> Result<(Attachment, Vec<u8>)> {
    let valid = id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !valid {
        return Err(not_found(id));
    }
    let file = ctx
        .store
        .attachment_by_id(id)
        .await?
        .ok_or_else(|| not_found(id))?;
    let bytes = ctx
        .blobs
        .get(&file.path)
        .await?
        .ok_or_else(|| not_found(id))?;
    Ok((file, bytes))
}

pub async fn delete(ctx: &Ctx, actor: &Actor, id: &str) -> Result<Attachment> {
    actor.require(Permission::PostModerate)?;
    let mut tx = Tx::begin(ctx).await?;
    let before = tx
        .attachment_by_id(id)
        .await?
        .ok_or_else(|| not_found(id))?;
    tx.delete_attachment(id).await?;
    tx.audit(
        actor,
        "file.delete",
        Target::new("file", id),
        audit::diff(Some(&before), None),
    )
    .await?;
    tx.commit().await?;
    // 기록이 지워진 뒤에 파일을 지운다. 실패하면 고아 파일만 남는다.
    let _ = ctx.blobs.delete(&before.path).await;
    Ok(before)
}

/// 경로·제어 문자를 지우고 길이를 줄인다. 한글 이름은 그대로 둔다.
fn clean_name(raw: &str) -> Result<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?'))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    if cleaned.is_empty() {
        return Err(Error::new(Code::Validation)
            .with_message("error.file_name")
            .arg("max", MAX_NAME.to_string()));
    }
    if cleaned.chars().count() <= MAX_NAME {
        return Ok(cleaned);
    }
    // 길면 확장자는 살리고 앞부분을 자른다.
    let (stem, ext) = cleaned.rsplit_once('.').unwrap_or((&cleaned, ""));
    let keep = MAX_NAME.saturating_sub(ext.chars().count() + 1);
    let stem: String = stem.chars().take(keep).collect();
    Ok(if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    })
}

/// 확장자와 내용 서명이 맞는지 본다. 맞으면 (확장자, MIME).
fn check(filename: &str, bytes: &[u8]) -> Result<(&'static str, &'static str)> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(Code::Validation)
            .with_message("error.media_too_large")
            .arg("max", (MAX_BYTES / 1024 / 1024).to_string()));
    }
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let Some(&(ext, mime, sniff)) = KINDS.iter().find(|k| k.0 == ext) else {
        return Err(Error::new(Code::Validation)
            .with_message("error.file_type")
            .arg("name", filename)
            .arg("allowed", allowed()));
    };
    let ok = match sniff {
        Sniff::Pdf => bytes.starts_with(b"%PDF-"),
        Sniff::Zip => bytes.starts_with(b"PK\x03\x04"),
        Sniff::Ole => bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]),
        Sniff::Text => !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok(),
    };
    if !ok {
        return Err(Error::new(Code::Validation)
            .with_message("error.file_content")
            .arg("name", filename)
            .arg("ext", ext));
    }
    Ok((ext, mime))
}

fn not_found(id: &str) -> Error {
    Error::new(Code::NotFound)
        .with_message("error.file_not_found")
        .arg("id", id.to_owned())
        .next("maru file list")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_paths_and_control_chars() {
        assert_eq!(clean_name("C:\\문서\\견적서.pdf").unwrap(), "견적서.pdf");
        assert_eq!(clean_name("../../etc/passwd.txt").unwrap(), "passwd.txt");
        assert_eq!(clean_name("a\"b<c>\n.pdf").unwrap(), "abc.pdf");
        assert_eq!(clean_name(".숨김.txt").unwrap(), "숨김.txt");
        assert!(clean_name("  ").is_err());
        let long = format!("{}.pdf", "가".repeat(300));
        let cut = clean_name(&long).unwrap();
        assert_eq!(cut.chars().count(), MAX_NAME);
        assert!(cut.ends_with(".pdf"));
    }

    #[test]
    fn extension_and_signature_must_agree() {
        assert_eq!(
            check("a.PDF", b"%PDF-1.7 ...").unwrap(),
            ("pdf", "application/pdf")
        );
        assert_eq!(check("b.hwpx", b"PK\x03\x04rest").unwrap().0, "hwpx");
        assert_eq!(
            check(
                "c.hwp",
                &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0]
            )
            .unwrap()
            .0,
            "hwp"
        );
        assert_eq!(check("메모.txt", "안녕".as_bytes()).unwrap().0, "txt");
        // HTML 을 PDF 로 꾸민 것, 이진 파일을 글자로 꾸민 것, 화이트리스트 밖 확장자
        for (name, bytes) in [
            ("x.pdf", &b"<html><script>alert(1)</script>"[..]),
            ("x.txt", &b"ab\0cd"[..]),
            ("x.docx", &b"%PDF-1.7"[..]),
            ("x.html", &b"<html>"[..]),
            ("x.svg", &b"<svg/>"[..]),
            ("x.exe", &b"MZ"[..]),
            ("noext", &b"%PDF-"[..]),
        ] {
            let e = check(name, bytes).unwrap_err();
            assert_eq!(e.code(), Code::Validation, "{name}");
        }
    }
}
