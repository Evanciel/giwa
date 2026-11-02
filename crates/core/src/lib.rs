//! Maru 도메인 모델, 서비스, 권한 (DB·웹 비의존)

pub mod config;
pub mod error;
pub mod messages;
pub mod output;

pub use error::{Code, Error, ErrorBody, ExitCode, Result};
