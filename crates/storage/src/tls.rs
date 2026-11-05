//! TLS 암호 엔진 (ADR-0083·0084). reqwest 는 프로세스 기본 엔진을 쓰므로 처음 한 번 ring 을 깐다.
//! lettre 는 같은 ring 엔진을 스스로 고른다.

/// 이미 깔려 있으면 그대로 둔다(여러 번 불러도 안전).
pub fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
