fn main() {
    let target = std::env::var("TARGET").expect("Cargo provides the build target");
    let sidecar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("wla-{target}.exe"));
    println!(
        "cargo:rerun-if-changed={}",
        sidecar.parent().expect("sidecar has a parent").display()
    );
    println!("cargo:rerun-if-changed={}", sidecar.display());

    // Rust checks and tests should work before the release sidecar has been prepared.
    // The packaging script also sets this while compiling wla itself, avoiding a
    // self-referential externalBin requirement.
    if !sidecar.is_file() || std::env::var_os("WONDERLAND_BUILD_WLA_SIDECAR").is_some() {
        // SAFETY: build scripts are single-threaded here and the override is read immediately
        // by tauri-build in this process only.
        unsafe {
            std::env::set_var("TAURI_CONFIG", r#"{"bundle":{"externalBin":[]}}"#);
        }
        println!("cargo:rerun-if-env-changed=WONDERLAND_BUILD_WLA_SIDECAR");
    }
    tauri_build::build()
}
