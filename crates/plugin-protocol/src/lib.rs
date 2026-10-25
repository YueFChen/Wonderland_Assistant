//! Core 与插件之间的稳定协议 DTO。
//!
//! 本 crate 不依赖 kernel、Tauri 或任何插件业务 crate，因此插件可独立依赖它。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
mod host;
pub use host::*;
mod lifecycle;
pub use lifecycle::*;
#[cfg(feature = "bindings")]
use ts_rs::TS;

pub const PROTOCOL_ID: &str = "wonderland-plugin";
pub const PROTOCOL_VERSION: &str = "1.0.0";
/// Protocol versions the current Core runtime can actually speak, in ascending order.
/// Keep this explicit so bumping `PROTOCOL_VERSION` cannot silently drop older support.
pub const PROTOCOL_SUPPORTED_VERSIONS: &[&str] = &["1.0.0"];
pub const UI_BRIDGE_PROTOCOL: &str = "wonderland-plugin-ui";
/// The newest UI bridge contract implemented by Core and the bundled UI SDK.
pub const UI_BRIDGE_VERSION: &str = "1.1.0";
/// List every bridge version this host still implements, in ascending order.
/// Keep the list explicit: a version bump must not silently remove old plugin compatibility.
pub const UI_BRIDGE_SUPPORTED_VERSIONS: &[&str] = &["1.0.0", "1.1.0"];
pub const MANIFEST_VERSION: u32 = 3;

/// Icons are decorative metadata. Invalid icon values must not make an otherwise valid plugin fail to load.
fn deserialize_optional_icon<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer).unwrap_or_default())
}

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
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// A legacy built-in icon key or a local package asset such as `asset:ui/icons/logo.svg`.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_icon",
        skip_serializing_if = "Option::is_none"
    )]
    pub icon: Option<String>,
    pub host_compatibility: HostCompatibility,
    pub platform: PluginPlatform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<PluginUi>,
    pub backend: PluginBackend,
    pub contract: String,
    /// Explicit opt-in to remote workspaces; absent or false means local only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_access: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<PluginDependency>,
}

impl PluginManifest {
    pub fn supports_remote_access(&self) -> bool {
        self.remote_access == Some(true) && self.backend.supports_service_context == Some(true)
    }
}

/// An expected plugin dependency, used for diagnostics and best-effort startup ordering.
/// Version and method expectations never act as a runtime call allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginDependency {
    pub plugin_id: String,
    /// A semver::VersionReq expression evaluated against the target plugin package version.
    pub version_req: String,
    pub optional: bool,
    pub methods: Vec<String>,
}

/// Parameters of the direct core.plugins.call host method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginCallParams {
    pub plugin_id: String,
    pub method: String,
    #[cfg_attr(feature = "bindings", ts(type = "unknown"))]
    pub params: Value,
    /// Optional deadline that can only shorten already declared ancestor/target deadlines.
    /// Missing or null does not add a deadline. Positive values have no policy upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", ts(as = "Option<f64>", optional))]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum LocalFileState {
    Saved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum RemoteFileState {
    Ready,
}

/// File delivery is completed locally or ready for the Core's remote download flow.
/// A remote result never asserts that the visiting device has saved a local path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum FileResult {
    Local {
        state: LocalFileState,
        name: String,
        #[cfg_attr(feature = "bindings", ts(type = "number"))]
        size: u64,
        path: String,
    },
    Remote {
        state: RemoteFileState,
        name: String,
        #[cfg_attr(feature = "bindings", ts(type = "number"))]
        size: u64,
        #[serde(rename = "downloadId")]
        download_id: String,
        /// Optional RFC 3339 timestamp at which the Core download reference expires.
        #[serde(default, rename = "expiresAt", skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "bindings", ts(optional))]
        expires_at: Option<String>,
    },
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
    /// A legacy built-in icon key or a local package asset such as `asset:ui/icons/logo.svg`.
    #[serde(
        default,
        deserialize_with = "deserialize_optional_icon",
        skip_serializing_if = "Option::is_none"
    )]
    pub icon: Option<String>,
    pub default_order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<PluginUiCommand>,
}

/// A command the plugin UI explicitly accepts from the Core CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginUiCommand {
    pub id: String,
    pub title: String,
    #[cfg_attr(feature = "bindings", ts(type = "unknown"))]
    pub input_schema: Value,
    pub effect: PluginUiCommandEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum PluginUiCommandEffect {
    ReadOnly,
    Mutating,
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
    /// Backend echoes Core-issued serviceContext on nested host calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_service_context: Option<bool>,
    /// Missing preserves legacy startup behavior; new plugins explicitly use on_demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<PluginActivationPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", ts(rename_all = "snake_case"))]
pub enum InstallationState {
    Missing,
    Installed,
    Checking,
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

/// Installation provenance shown to the user; this is not a publisher signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginInstallSource {
    pub kind: String,
    pub origin: String,
    pub author: Option<String>,
}

/// Diagnostics from validating an installed plugin package.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginSignatureState {
    pub state: String,
    pub public_key: Option<String>,
    pub expected_public_key: Option<String>,
}

impl Default for PluginSignatureState {
    fn default() -> Self {
        Self {
            state: "unsigned".into(),
            public_key: None,
            expected_public_key: None,
        }
    }
}

/// Diagnostics from validating an installed plugin package.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginScanDiagnostic {
    pub core_version: String,
    pub code: String,
    pub phase: String,
    pub message: String,
    pub retry_attempt: u8,
    pub retry_limit: u8,
    pub retrying: bool,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(TS))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "bindings", ts(rename_all = "camelCase"))]
pub struct PluginRuntimeState {
    pub manifest: PluginManifest,
    #[serde(default)]
    pub signature: PluginSignatureState,
    pub installation: InstallationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_diagnostic: Option<PluginScanDiagnostic>,
    pub enabled: bool,
    pub runtime: RuntimeState,
    pub last_error: Option<PluginFailure>,
    #[serde(default)]
    pub installation_source: Option<PluginInstallSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_issues: Vec<String>,
    /// Expected per-plugin persistent data directory managed by Core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_data_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<PluginLifecycleState>,
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
    /// Opaque Core-issued invocation context, echoed by SDK host-service calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_context: Option<String>,
    /// Optional explicit deadline for this host-service request; absent/null is unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "bindings", ts(as = "Option<f64>", optional))]
    pub timeout_ms: Option<u64>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Value {
        json!({
            "manifestVersion": 3,
            "id": "consumer",
            "name": "Consumer",
            "version": "0.1.0",
            "hostCompatibility": {
                "minCoreVersion": "0.1.9",
                "maxCoreVersionExclusive": "1.0.0",
                "protocol": { "minVersion": "1.0.0", "maxVersionExclusive": "2.0.0" }
            },
            "platform": { "os": "windows", "architecture": "x86_64", "abi": "msvc" },
            "backend": { "entry": "backend/plugin.exe", "transport": "stdio-ndjson-v1" },
            "contract": "contract.json"
        })
    }

    #[test]
    fn current_manifest_rejects_removed_policy_and_service_fields() {
        let current: PluginManifest = serde_json::from_value(manifest()).unwrap();
        assert!(current.dependencies.is_empty());
        for field in ["capabilities", "networkPublicHosts", "provides", "requires"] {
            let mut legacy = manifest();
            legacy[field] = json!([]);
            assert!(serde_json::from_value::<PluginManifest>(legacy).is_err());
        }
    }

    #[test]
    fn dependency_metadata_keeps_the_target_plugin_and_required_field_shape() {
        let dependency = json!({
            "pluginId": "provider",
            "versionReq": ">=0.1.3, <1.0.0",
            "optional": true,
            "methods": []
        });
        let parsed: PluginDependency = serde_json::from_value(dependency.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), dependency);
        for field in ["pluginId", "versionReq", "optional", "methods"] {
            let mut missing = dependency.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<PluginDependency>(missing).is_err());
        }
    }

    #[test]
    fn direct_calls_do_not_accept_the_removed_service_id() {
        let request = json!({ "pluginId": "provider", "method": "read", "params": {} });
        let parsed: PluginCallParams = serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), request);
        let mut legacy = request;
        legacy["serviceId"] = json!("wonderland.archive");
        assert!(serde_json::from_value::<PluginCallParams>(legacy).is_err());
    }

    #[test]
    fn service_requests_keep_optional_deadlines_out_of_legacy_frames() {
        let base = json!({"protocol":PROTOCOL_ID,"version":PROTOCOL_VERSION,"type":"request", "id":"p-1","method":"core.network.request","params":{}});
        for timeout in [None, Some(Value::Null), Some(json!(172_800_000_u64))] {
            let mut value = base.clone();
            if let Some(timeout) = timeout {
                value["timeoutMs"] = timeout;
            }
            let parsed: PluginRequest = serde_json::from_value(value).unwrap();
            if parsed.timeout_ms.is_none() {
                assert_eq!(serde_json::to_value(parsed).unwrap(), base);
            } else {
                assert_eq!(parsed.timeout_ms, Some(172_800_000));
            }
        }
    }

    #[test]
    fn file_delivery_states_and_fields_cannot_be_mixed() {
        let local = json!({
            "kind": "local", "state": "saved", "name": "report.csv", "size": 128,
            "path": "D:\\Exports\\report.csv"
        });
        let remote = json!({
            "kind": "remote", "state": "ready", "name": "report.csv", "size": 128,
            "downloadId": "opaque-id"
        });
        for value in [local.clone(), remote.clone()] {
            let parsed: FileResult = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        }
        let mut wrong_state = local;
        wrong_state["state"] = json!("ready");
        assert!(serde_json::from_value::<FileResult>(wrong_state).is_err());
        let mut remote_path = remote;
        remote_path["path"] = json!("D:\\remote.csv");
        assert!(serde_json::from_value::<FileResult>(remote_path).is_err());
    }

    #[test]
    fn host_hello_has_no_grant_negotiation() {
        let hello = json!({
            "protocol": PROTOCOL_ID, "version": PROTOCOL_VERSION, "type": "hello",
            "role": "host", "pluginId": "consumer", "coreVersion": "0.1.9"
        });
        let parsed: HostHello = serde_json::from_value(hello.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), hello);
        let mut legacy = hello;
        legacy["grantedCapabilities"] = json!([]);
        assert!(serde_json::from_value::<HostHello>(legacy).is_err());
    }
}
