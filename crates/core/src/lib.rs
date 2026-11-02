//! Maru 도메인 모델, 서비스, 권한 (DB·웹 비의존)

pub mod actor;
pub mod audit;
pub mod clock;
pub mod config;
pub mod error;
pub mod markdown;
pub mod messages;
pub mod model;
pub mod output;
pub mod services;
pub mod slug;
pub mod store;
pub mod urls;

pub use actor::{Actor, Permission, Scope};
pub use error::{Code, Error, ErrorBody, ExitCode, Result};
pub use store::{Ctx, Store};
