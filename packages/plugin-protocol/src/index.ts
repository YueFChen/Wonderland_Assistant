// Generated from Rust by crates/plugin-protocol/examples/generate_bindings.rs. Do not edit.
export type PluginManifest = { manifestVersion: number, id: string, name: string, version: string, description?: string | null, icon?: string | null, hostCompatibility: HostCompatibility, platform: PluginPlatform, ui?: PluginUi | null, backend: PluginBackend, contract: string, capabilities: Array<string>, provides?: Array<PluginService>, requires?: Array<PluginServiceRequirement>, };
export type PluginService = { id: string, version: string, methods: Array<string>, };
export type PluginServiceRequirement = { id: string, minVersion: string, maxVersionExclusive: string, optional: boolean, };
export type HostCompatibility = { minCoreVersion: string, maxCoreVersionExclusive: string, protocol: ProtocolCompatibility, };
export type ProtocolCompatibility = { minVersion: string, maxVersionExclusive: string, };
export type PluginPlatform = { os: string, architecture: string, abi: string, };
export type PluginUi = { entry: string, bridgeCompatibility: ProtocolCompatibility, integrations: Array<string>, contributions: Array<PluginUiContribution>, };
export type PluginUiContribution = { id: string, kind: PluginUiContributionKind, title: string, icon?: string | null, defaultOrder: number, location?: string | null, };
export type PluginUiContributionKind = "activity" | "view";
export type PluginBackend = { entry: string, transport: string, };
export type InstallationState = "missing" | "installed" | "invalid" | "incompatible";
export type RuntimeState = "stopped" | "starting" | "running" | "stopping" | "failed";
export type PluginFailure = { code: string, message: string, 
/**
 * RFC 3339 UTC timestamp.
 */
occurredAt: string, };
export type PluginRuntimeState = { manifest: PluginManifest, installation: InstallationState, enabled: boolean, runtime: RuntimeState, lastError: PluginFailure | null, grantedCapabilities: Array<string>, serviceDependencyIssues?: Array<string>, };
export type PluginError = { code: string, message: string, details: unknown, };
export type HostHello = { protocol: string, version: string, type: string, role: string, pluginId: string, coreVersion: string, grantedCapabilities: Array<string>, };
export type PluginHello = { protocol: string, version: string, type: string, role: string, pluginId: string, pluginVersion: string, contractSha256: string, };
export type PluginRequest = { protocol: string, version: string, type: string, id: string, method: string, params: unknown, };
export type PluginResult = { protocol: string, version: string, type: string, id: string, result: unknown, };
export type PluginErrorMessage = { protocol: string, version: string, type: string, id: string, error: PluginError, };
export type PluginEvent = { protocol: string, version: string, type: string, topic: string, payload: unknown, requestId: string | null, };
export type PluginCancel = { protocol: string, version: string, type: string, id: string, };
export type PluginMessage = HostHello | PluginHello | PluginRequest | PluginResult | PluginErrorMessage | PluginEvent | PluginCancel;
