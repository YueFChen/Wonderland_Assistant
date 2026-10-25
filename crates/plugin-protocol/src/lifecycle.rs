//! Activation metadata is separate from availability, process state and business tasks.
use serde::{Deserialize, Serialize};
#[cfg(feature = "bindings")]
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum PluginActivationPolicy {
    OnDemand,
    BackgroundRestore,
    OnStartup,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum BackgroundRestoreState {
    #[default]
    Unknown,
    NotRequired,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginLifecycleState {
    pub policy: PluginActivationPolicy,
    pub policy_source: String,
    pub background_restore: BackgroundRestoreState,
    pub active_activities: u32,
    pub last_activation_reason: Option<String>,
    pub diagnostic: Option<String>,
    /// Changes only on a committed package replacement, never on a backend retry.
    pub package_revision: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct BackgroundRestoreParams {
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct ActivityBeginParams {
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct ActivityHandle {
    pub activity_id: String,
}
