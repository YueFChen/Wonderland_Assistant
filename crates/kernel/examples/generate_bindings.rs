//! Generate TypeScript bindings for the kernel contracts.
use ts_rs::{Config, TS};
use wonderland_kernel::logging::{LogLevel, LogSettings};
use wonderland_kernel::*;

const HEADER: &str =
    "// Generated from Rust by crates/kernel/examples/generate_bindings.rs. Do not edit.\n";

fn main() {
    let cfg = Config::default();
    let mut out = String::from(HEADER);
    macro_rules! emit { ($($t:ty),*) => { $(out.push_str("export ");out.push_str(&<$t>::decl(&cfg));out.push('\n');)* }; }
    emit!(
        AccountStatus,
        LoginCancelReason,
        GameRole,
        Account,
        AccountSnapshot,
        ErrorPayload,
        ThemeMode,
        BackgroundKind,
        BackgroundAsset,
        ThemeSettings,
        ThemeState,
        LogLevel,
        LogSettings,
        UserDataLocationKind,
        UserDataState
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/core-bindings/src/index.ts");
    if std::env::args().any(|a| a == "--check") {
        assert_eq!(
            std::fs::read_to_string(path).expect("generated file"),
            out,
            "Bindings drift: regenerate bindings"
        );
    } else {
        std::fs::write(path, out).unwrap();
    }
}
