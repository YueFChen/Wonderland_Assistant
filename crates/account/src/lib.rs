//! 账号凭据验证、加密存储和账号状态管理。

pub mod cookie;
mod service;
mod store;

pub use cookie::{Credentials, evaluate, select_mihoyo_cookies};
pub use service::FsAccountService;
