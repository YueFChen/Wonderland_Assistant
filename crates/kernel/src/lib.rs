//! Wonderland Assistant 的稳定内核接口与共享类型。

pub mod account;
pub mod context;
pub mod error;
pub mod logging;
pub mod settings;
pub mod theme;
pub mod user_data;

pub use account::{
    Account, AccountService, AccountSnapshot, AccountStatus, GameRole, LoginCancelReason,
    LoginOutcome, StoredCookie,
};
pub use context::AppContext;
pub use error::{ErrorPayload, KernelError};
pub use settings::SettingsService;
pub use theme::{BackgroundAsset, BackgroundKind, ThemeMode, ThemeSettings, ThemeState};
pub use user_data::{UserDataLocationKind, UserDataService, UserDataState};
