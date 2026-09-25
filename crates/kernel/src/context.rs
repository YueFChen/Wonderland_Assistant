use std::path::PathBuf;
use std::sync::Arc;

use crate::account::AccountService;
use crate::error::KernelError;
use crate::settings::SettingsService;
use crate::user_data::UserDataService;

/// 由宿主组装并提供给插件服务的依赖容器。
pub struct AppContext {
    /// 应用数据目录。
    pub app_data_dir: PathBuf,
    /// 用于存放用户可访问导出文件的目录。
    pub documents_dir: PathBuf,
    /// 账号服务；未启用时为 `None`。
    pub account: Option<Arc<dyn AccountService>>,
    /// 应用设置服务。
    pub settings: Option<Arc<dyn SettingsService>>,
    /// 用户数据目录管理。测试或无持久化入口可不装。
    pub user_data: Option<Arc<dyn UserDataService>>,
}

impl AppContext {
    /// 取账号服务；未启用时返回 [`KernelError::NotInitialized`]。
    pub fn account(&self) -> Result<&Arc<dyn AccountService>, KernelError> {
        self.account.as_ref().ok_or(KernelError::NotInitialized)
    }

    /// 取设置服务；宿主未组装时返回 [`KernelError::NotInitialized`]。
    pub fn settings(&self) -> Result<&Arc<dyn SettingsService>, KernelError> {
        self.settings.as_ref().ok_or(KernelError::NotInitialized)
    }

    pub fn user_data(&self) -> Result<&Arc<dyn UserDataService>, KernelError> {
        self.user_data.as_ref().ok_or(KernelError::NotInitialized)
    }
}
