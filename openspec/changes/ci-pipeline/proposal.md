## Why

The project documents quality gates (`cargo fmt --check`, `cargo clippy -- -D warnings`,
`cargo test`) but nothing runs them automatically — there is no `.github/` directory. The
current branch already silently violates its own rules: `cargo fmt --check`, the lib clippy
pass, and the `--all-targets` clippy pass all fail at HEAD. Two agreed conventions are also
unenforced: the documented clippy command omits `--all-targets` (so test-code lints slip
through), and CONVENTIONS.md #6 ("no `unwrap`/`expect` outside tests") has no machine check.

## What Changes

- Add a GitHub Actions workflow that runs on `pull_request` and `push` to `main`: `cargo fmt
  --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace`,
  `cargo test --workspace`. Strict: any warning fails the build. No delivery/release stage.
- Fix the 4 existing violations so the default lint set is green: `server.rs` formatting;
  `config.rs` `<= 0` → `== 0`; move `run_server` above `mod tests`; `integration.rs`
  `"".to_string()` → `String::new()`.
- Enforce convention #6 in the lint config: add `clippy::unwrap_used` / `clippy::expect_used`
  (warn), with `cfg(test)` allows for test code and justified per-site allows for the 3
  documented production `expect` sites.
- Update `CLAUDE.md` to use `--all-targets` in the documented clippy command; note #6 is now
  CI-enforced in `CONVENTIONS.md`.
- **Deferred to a follow-up change** (not in this one): enforcing `missing_docs` (convention
  #4), which requires authoring ~48 contract doc-comments plus an allow for generated proto code.

## Capabilities

### New Capabilities
- `continuous-integration`: Automated quality gates (format, lint, build, test) that run on
  pull requests and pushes to `main`, plus the lint-configuration policy that enforces the
  machine-checkable coding conventions.

### Modified Capabilities
<!-- None: openspec/specs/ contains no existing capabilities. -->

## Impact

- **New**: `.github/workflows/ci.yml`.
- **Config**: `Cargo.toml` workspace lints (`unwrap_used`, `expect_used`).
- **Code (mechanical)**: `crates/master/src/{config.rs,server.rs}`,
  `crates/master/tests/integration.rs`, `crates/{agent,master,proto}/src/lib.rs` (test allows),
  `crates/proto/src/lib.rs` + `crates/master/src/registry.rs` (3 per-site `expect` allows).
- **Docs**: `CLAUDE.md`, `CONVENTIONS.md`.
- **Dependencies**: no new Rust deps; CI uses `actions/checkout` and
  `actions-rust-lang/setup-rust-toolchain` (reads `rust-toolchain.toml`, bundles caching). No
  `protoc` needed — `crates/proto/build.rs` uses `protoc-bin-vendored`.
