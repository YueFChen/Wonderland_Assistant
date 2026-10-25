# Direct plugin call fixture

Independent Rust workspace for isolated Core integration tests. It does not load real user data or launch the desktop application.

Build with `cargo build --offline --manifest-path tests/fixtures/direct-plugin/Cargo.toml`. The fixture inherits Core's `.cargo/config.toml`; the executable is `target/debug/wonderland-direct-plugin-fixture.exe` relative to the Core checkout.

Use the same executable and `contract.json` for three plugins with ID `a`, `b`, and `c`, version `0.0.0`. Set each process's `WONDERLAND_PLUGIN_DATA_DIR` to an isolated directory ending in that ID. SDK handshake advertises service-context support and preserves the received context through the chain.

Calling `a.echo` with `{"text":"fixture"}` calls `b.echo`, then `c.echo`, which returns `"fixture"`. Core tests supply manifests, enabled state, dependency declarations and remote sharing choices; the fixture does not impose them.

The fixture also implements `direct`, `loop`, `delay`, `health`, `pick`, `browser` and `bad_result`. These cover real nested IPC, bounded cycles, inherited deadlines and cancellation, native interaction failures and actual result-schema rejection. Its contract must match the built executable; rebuild after changing `contract.json`.

An additional isolated ID `blocked` acknowledges the Core handshake and then stops reading stdin. The `large` contract accepts a 2 MiB text frame with a one-second timeout. Core tests use this owned child to verify bounded pipe-writer buffering, request timeouts, cancellation without holding manager state, upgrade blockers while a write remains outstanding, and child collection on shutdown.

Run the complete manager process suite with `cargo test -p wonderland-desktop --lib plugin_manager:: --locked -- --ignored --test-threads=1`. It includes direct calls, lifecycle activation, rollback and batch installation. Run `cargo test -p wonderland-desktop --lib web_access::tests::cold_remote --locked -- --ignored --test-threads=1` for the HTTP path. See the [test guide](../../README.md) for the complete check sequence. These tests install temporary packages for `a`, `b`, `c` and `blocked`, use a headless environment, and delete only their temporary data after stopping their processes. Set `WLA_DIRECT_PLUGIN_FIXTURE` when the executable is outside the default target directory. Tests do not build the fixture automatically.
