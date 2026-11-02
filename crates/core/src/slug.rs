//! 슬러그. 한글을 허용하고 NFC 로 정규화한다 (PHASE1_PLAN §4.7).

use unicode_normalization::UnicodeNormalization;

use crate::error::{Code, Error, Result};

const MAX_CHARS: usize = 80;

/// 사용자가 준 슬러그를 검증한다. 대문자는 소문자로, 공백은 `-` 로 바꾼다.
pub fn normalize(input: &str) -> Result<String> {
    let s: String = input.trim().nfc().collect();
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '_' {
            out.push('-');
        } else if ch.is_alphanumeric() || ch == '-' {
            out.extend(ch.to_lowercase());
        } else {
            return Err(invalid(input));
        }
    }
    let out = collapse(&out);
    if out.is_empty() || out.chars().count() > MAX_CHARS {
        return Err(invalid(input));
    }
    Ok(out)
}

/// 제목에서 슬러그를 만든다. 허용하지 않는 글자는 `-` 로 바꾼다. 비면 `fallback`.
pub fn from_title(title: &str, fallback: &str) -> String {
    let s: String = title.nfc().collect();
    let mapped: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .flat_map(char::to_lowercase)
        .collect();
    let out: String = collapse(&mapped).chars().take(MAX_CHARS).collect();
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        fallback.to_owned()
    } else {
        out
    }
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(ch);
    }
    out.trim_end_matches('-').to_owned()
}

fn invalid(input: &str) -> Error {
    Error::new(Code::Validation)
        .with_message("error.slug_invalid")
        .arg("value", input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hangul_is_kept_and_nfc() {
        // 분해형(NFD) 한글 입력
        let nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}";
        assert_eq!(normalize(nfd).unwrap(), "한글");
        assert_eq!(normalize("  Hello World_2 ").unwrap(), "hello-world-2");
    }

    #[test]
    fn rejects_unsafe() {
        for bad in ["", "a/b", "../x", "a?b", "<script>", "-"] {
            assert!(normalize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn from_title_maps_symbols() {
        assert_eq!(
            from_title("첫 글: Rust로 CMS 만들기!", "post"),
            "첫-글-rust로-cms-만들기"
        );
        assert_eq!(from_title("!!!", "post"), "post");
    }
}
