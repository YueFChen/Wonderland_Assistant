// Generated from Rust by crates/plugin-protocol/examples/generate_bindings.rs. Do not edit.
export const UI_BRIDGE_PROTOCOL = "wonderland-plugin-ui" as const
export const UI_BRIDGE_VERSION = "1.1.0" as const
export const MANIFEST_VERSION = 3 as const
export type PluginManifest = { manifestVersion: number, id: string, name: string, version: string, description?: string | null, author?: string | null, homepage?: string | null, license?: string | null,
/**
 * A legacy built-in icon key or a local package asset such as `asset:ui/icons/logo.svg`.
 */
icon?: string | null, hostCompatibility: HostCompatibility, platform: PluginPlatform, ui?: PluginUi | null, backend: PluginBackend, contract: string,
/**
 * Explicit opt-in to remote workspaces; absent or false means local only.
 */
remoteAccess?: boolean | null, dependencies?: Array<PluginDependency>, };
export type PluginDependency = { pluginId: string,
/**
 * A semver::VersionReq expression evaluated against the target plugin package version.
 */
versionReq: string, optional: boolean, methods: Array<string>, };
export type PluginCallParams = { pluginId: string, method: string, params: unknown,
/**
 * Optional deadline that can only shorten already declared ancestor/target deadlines.
 * Missing or null does not add a deadline. Positive values have no policy upper bound.
 */
timeoutMs?: number, };
export type ByteStream = { streamId: string, size?: number, };
export type StreamChunk = { contentBase64: string, eof: boolean, };
export type HttpHeader = { name: string, value: Array<number>, };
export type HttpBody = { "kind": "text", content: string, } | { "kind": "bytes", contentBase64: string, } | { "kind": "file", path: string, } | { "kind": "stream", streamId: string, };
export type SecretHeader = { secretId: string, name: string, prefix: string, };
export type HttpRedirects = { follow: boolean, maxRedirects?: number, };
export type HttpRequestParams = { url: string, method: string, headers: Array<HttpHeader>, query: Array<[string, string]>, body?: HttpBody | null, authentication?: SecretHeader | null, redirects: HttpRedirects, timeoutMs?: number, };
export type HttpResponseMeta = { status: number, headers: Array<HttpHeader>, finalUrl: string, body: ByteStream, };
export type FileWriteMode = "create" | "replace";
export type LocalFileState = "saved";
export type RemoteFileState = "ready";
export type FileResult = { "kind": "local", state: LocalFileState, name: string, size: number, path: string, } | { "kind": "remote", state: RemoteFileState, name: string, size: number, downloadId: string,
/**
 * Optional RFC 3339 timestamp at which the Core download reference expires.
 */
expiresAt?: string, };
export type HostCompatibility = { minCoreVersion: string, maxCoreVersionExclusive: string, protocol: ProtocolCompatibility, };
export type ProtocolCompatibility = { minVersion: string, maxVersionExclusive: string, };
export type PluginPlatform = { os: string, architecture: string, abi: string, };
export type PluginUi = { entry: string, bridgeCompatibility: ProtocolCompatibility, integrations: Array<string>, contributions: Array<PluginUiContribution>, };
export type PluginUiContribution = { id: string, kind: PluginUiContributionKind, title: string,
/**
 * A legacy built-in icon key or a local package asset such as `asset:ui/icons/logo.svg`.
 */
icon?: string | null, defaultOrder: number, location?: string | null, commands?: Array<PluginUiCommand>, };
export type PluginUiContributionKind = "activity" | "view";
export type PluginUiCommand = { id: string, title: string, inputSchema: unknown, effect: PluginUiCommandEffect, };
export type PluginUiCommandEffect = "read_only" | "mutating";
export type PluginBackend = { entry: string, transport: string,
/**
 * Backend echoes Core-issued serviceContext on nested host calls.
 */
supportsServiceContext?: boolean | null,
/**
 * Missing preserves legacy startup behavior; new plugins explicitly use on_demand.
 */
activation?: PluginActivationPolicy | null, };
export type PluginActivationPolicy = "on_demand" | "background_restore" | "on_startup";
export type BackgroundRestoreState = "unknown" | "not_required" | "required";
export type PluginLifecycleState = { policy: PluginActivationPolicy, policySource: string, backgroundRestore: BackgroundRestoreState, activeActivities: number, lastActivationReason: string | null, diagnostic: string | null,
/**
 * Changes only on a committed package replacement, never on a backend retry.
 */
packageRevision: number, };
export type BackgroundRestoreParams = { required: boolean, };
export type ActivityBeginParams = { label: string | null, };
export type ActivityHandle = { activityId: string, };
export type InstallationState = "missing" | "installed" | "checking" | "invalid" | "incompatible";
export type RuntimeState = "stopped" | "starting" | "running" | "stopping" | "failed";
export type PluginFailure = { code: string, message: string,
/**
 * RFC 3339 UTC timestamp.
 */
occurredAt: string, };
export type PluginInstallSource = { kind: string, origin: string, author: string | null, };
export type PluginSignatureState = { state: string, publicKey: string | null, expectedPublicKey: string | null, };
export type PluginScanDiagnostic = { coreVersion: string, code: string, phase: string, message: string, retryAttempt: number, retryLimit: number, retrying: boolean, retryable: boolean, };
export type PluginRuntimeState = { manifest: PluginManifest, signature: PluginSignatureState, installation: InstallationState, scanDiagnostic?: PluginScanDiagnostic | null, enabled: boolean, runtime: RuntimeState, lastError: PluginFailure | null, installationSource: PluginInstallSource | null, dependencyIssues?: Array<string>,
/**
 * Expected per-plugin persistent data directory managed by Core.
 */
pluginDataDirectory?: string | null, lifecycle?: PluginLifecycleState | null, };
export type PluginError = { code: string, message: string, details: unknown, };
export type HostHello = { protocol: string, version: string, type: string, role: string, pluginId: string, coreVersion: string, };
export type PluginHello = { protocol: string, version: string, type: string, role: string, pluginId: string, pluginVersion: string, contractSha256: string, };
export type PluginRequest = { protocol: string, version: string, type: string, id: string, method: string, params: unknown,
/**
 * Opaque Core-issued invocation context, echoed by SDK host-service calls.
 */
serviceContext?: string | null,
/**
 * Optional explicit deadline for this host-service request; absent/null is unbounded.
 */
timeoutMs?: number, };
export type PluginResult = { protocol: string, version: string, type: string, id: string, result: unknown, };
export type PluginErrorMessage = { protocol: string, version: string, type: string, id: string, error: PluginError, };
export type PluginEvent = { protocol: string, version: string, type: string, topic: string, payload: unknown, requestId: string | null, };
export type PluginCancel = { protocol: string, version: string, type: string, id: string, };
export type PluginMessage = HostHello | PluginHello | PluginRequest | PluginResult | PluginErrorMessage | PluginEvent | PluginCancel;
