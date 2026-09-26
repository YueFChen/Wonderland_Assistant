//! Generate the protocol TypeScript declarations consumed by Core and plugin SDK packages.
use ts_rs::{Config, TS};
use wonderland_plugin_protocol::*;

const HEADER: &str = "// Generated from Rust by crates/plugin-protocol/examples/generate_bindings.rs. Do not edit.\n";

fn main() {
    let cfg = Config::default();
    let mut output = String::from(HEADER);
    macro_rules! emit {
        ($($ty:ty),* $(,)?) => { $(output.push_str("export "); output.push_str(&<$ty>::decl(&cfg)); output.push('\n');)* };
    }
    emit!(
        PluginManifest,
        PluginService,
        PluginServiceRequirement,
        PluginServiceResolution,
        HostCompatibility,
        ProtocolCompatibility,
        PluginPlatform,
        PluginUi,
        PluginUiContribution,
        PluginUiContributionKind,
        PluginUiCommand,
        PluginUiCommandEffect,
        PluginBackend,
        InstallationState,
        RuntimeState,
        PluginFailure,
        PluginInstallSource,
        PluginRuntimeState,
        PluginError,
        HostHello,
        PluginHello,
        PluginRequest,
        PluginResult,
        PluginErrorMessage,
        PluginEvent,
        PluginCancel,
        PluginMessage,
    );
    output = output
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/plugin-protocol/src/index.ts");
    if std::env::args().any(|arg| arg == "--check") {
        assert_eq!(
            std::fs::read_to_string(path).expect("generated file"),
            output,
            "Bindings drift: regenerate plugin protocol bindings"
        );
    } else {
        std::fs::write(path, output).unwrap();
    }
}
