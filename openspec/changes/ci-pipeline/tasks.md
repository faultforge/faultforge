## 1. Fix existing violations (make default lint set green)

- [x] 1.1 Run `cargo fmt` to fix formatting in `crates/master/src/server.rs`
- [x] 1.2 In `crates/master/src/config.rs:50`, change `<= 0` to `== 0` (behavior-preserving on unsigned type)
- [x] 1.3 In `crates/master/src/server.rs`, move `pub async fn run_server` above `mod tests` (fixes `items_after_test_module`)
- [x] 1.4 In `crates/master/tests/integration.rs:246`, change `"".to_string()` to `String::new()`
- [x] 1.5 Verify `cargo clippy --workspace --all-targets -- -D warnings` passes before adding new lints

## 2. Enforce convention #6 (no unwrap/expect outside tests)

- [x] 2.1 In root `Cargo.toml` `[workspace.lints.clippy]`, add `unwrap_used = "warn"` and `expect_used = "warn"`
- [x] 2.2 Add `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]` to `crates/agent/src/lib.rs`, `crates/master/src/lib.rs`, `crates/proto/src/lib.rs`
- [x] 2.3 Add crate-level `#![allow(clippy::unwrap_used, clippy::expect_used)]` to `crates/master/tests/integration.rs` (integration crate has no `cfg(test)`)
- [x] 2.4 Add per-site `#[allow(clippy::expect_used)]` with justification to the 3 production sites: `proto/src/lib.rs:25`, `master/src/registry.rs:32`, `master/src/registry.rs:46`
- [x] 2.5 Verify `cargo clippy --workspace --all-targets -- -D warnings` still passes

## 3. CI workflow

- [x] 3.1 Create `.github/workflows/ci.yml` triggering on `pull_request` and `push` to `main`
- [x] 3.2 Single `ubuntu-latest` job: `actions/checkout@v4` then `actions-rust-lang/setup-rust-toolchain@v1` (reads `rust-toolchain.toml`, caching on)
- [x] 3.3 Add discrete steps: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, `cargo test --workspace`
- [x] 3.4 Confirm no `protoc` install step is present (vendored via `protoc-bin-vendored`)

## 4. Docs

- [x] 4.1 In `CLAUDE.md` "Commands", change the clippy line to `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 4.2 Add a line in `CONVENTIONS.md` noting rule #6 is now CI-enforced

## 5. Verify

- [x] 5.1 Locally confirm green: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`, `cargo test --workspace`
- [ ] 5.2 Push branch / open PR and confirm the workflow runs and goes green; iterate the YAML if the runner differs from local
