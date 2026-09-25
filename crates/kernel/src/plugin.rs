use crate::{AppContext, KernelError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub route: String,
    #[serde(default)]
    pub icon: String,
}

pub trait Plugin: Sized {
    fn manifest() -> PluginManifest;
    fn init(context: &AppContext) -> Result<Self, KernelError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
pub struct PluginState {
    pub manifest: PluginManifest,
    pub enabled: bool,
    pub next_start_enabled: bool,
    pub startup_error: Option<String>,
}
