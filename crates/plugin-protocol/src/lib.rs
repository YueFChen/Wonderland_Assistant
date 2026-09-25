//! Core 与插件之间的稳定协议 DTO。
//!
//! 本 crate 不依赖 kernel、Tauri 或任何插件业务 crate，因此插件可独立依赖它。

use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(feature = "bindings")]
use ts_rs::TS;

pub const PROTOCOL_ID: &str = "wonderland-plugin";
pub const PROTOCOL_VERSION: &str = "1.0.0";
pub const UI_BRIDGE_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginManifest {
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub host_compatibility: HostCompatibility,
    pub platform: PluginPlatform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<PluginUi>,
    pub backend: PluginBackend,
    pub contract: String,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<PluginService>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<PluginServiceRequirement>,
}

/// A versioned service contract offered to other plugins through a future Core broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginService {
    pub id: String,
    pub version: String,
    pub methods: Vec<String>,
}

/// A service contract range required by this plugin. `optional` lets the plugin report
/// reduced functionality without preventing its backend from starting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginServiceRequirement {
    pub id: String,
    pub min_version: String,
    pub max_version_exclusive: String,
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct HostCompatibility {
    pub min_core_version: String,
    pub max_core_version_exclusive: String,
    pub protocol: ProtocolCompatibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct ProtocolCompatibility {
    pub min_version: String,
    pub max_version_exclusive: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginPlatform {
    pub os: String,
    pub architecture: String,
    pub abi: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginUi {
    pub entry: String,
    pub bridge_compatibility: ProtocolCompatibility,
    pub integrations: Vec<String>,
    pub contributions: Vec<PluginUiContribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginUiContribution {
    pub id: String,
    pub kind: PluginUiContributionKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub default_order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum PluginUiContributionKind {
    Activity,
    View,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginBackend {
    pub entry: String,
    pub transport: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum InstallationState {
    Missing,
    Installed,
    Invalid,
    Incompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum RuntimeState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginFailure {
    pub code: String,
    pub message: String,
    /// RFC 3339 UTC timestamp.
    pub occurred_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginRuntimeState {
    pub manifest: PluginManifest,
    pub installation: InstallationState,
    pub enabled: bool,
    pub runtime: RuntimeState,
    pub last_error: Option<PluginFailure>,
    pub granted_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_dependency_issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginError {
    pub code: String,
    pub message: String,
    #[cfg_attr(feature = "bindings", ts(type = "unknown"))]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct HostHello {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub role: String,
    pub plugin_id: String,
    pub core_version: String,
    pub granted_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginHello {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub role: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub contract_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginRequest {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub id: String,
    pub method: String,
    #[cfg_attr(feature = "bindings", ts(type = "unknown"))]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginResult {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub id: String,
    #[cfg_attr(feature = "bindings", ts(type = "unknown"))]
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginErrorMessage {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub id: String,
    pub error: PluginError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginEvent {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub topic: String,
    #[cfg_attr(feature = "bindings", ts(type = "unknown"))]
    pub payload: Value,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginCancel {
    pub protocol: String,
    pub version: String,
    #[serde(rename = "type")]
    #[cfg_attr(feature = "bindings", ts(rename = "type"))]
    pub message_type: String,
    pub id: String,
}

/// v1 wire messages are flat JSON objects; this untagged union keeps the exact wire layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(untagged)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub enum PluginMessage {
    HostHello(HostHello),
    PluginHello(PluginHello),
    Request(PluginRequest),
    Result(PluginResult),
    Error(PluginErrorMessage),
    Event(PluginEvent),
    Cancel(PluginCancel),
}
