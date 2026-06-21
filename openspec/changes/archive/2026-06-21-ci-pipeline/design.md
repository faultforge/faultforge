/## Context

FaultForge is a Cargo workspace (4 crates) with documented quality commands but no automation —
there is no `.github/` directory. At the current branch HEAD the checks already fail:

| Check | Status at HEAD |
|-------|----------------|
| `cargo fmt --check` | FAIL — 2 spots in `crates/master/src/server.rs` |
| `cargo clippy --workspace -- -D warnings` | FAIL — `absurd_extreme_comparisons` at `config.rs:50` |
| `cargo clippy --workspace --all-targets -- -D warnings` | FAIL — `items_after_test_module` (`server.rs`), `manual_string_new` (`integration.rs:246`) |
| `cargo test --workspace` | pass |

Measured read-only before designing: production (non-test) code has only **3 `expect()` sites**
(`proto/src/lib.rs:25`, `master/src/registry.rs:32`, `master/src/registry.rs:46`) and **zero
`unwrap`**. The remaining ~65 unwrap/expect uses are all in test code. Remote
`origin = github.com/faultforge/faultforge` exists, so the workflow will run once pushed.

## Goals / Non-Goals

**Goals:**
- Automated, strict (fail-on-any-warning) format/lint/build/test gates on PRs and `main`.
- Make the repo green under the default lint set by fixing the 4 existing violations.
- Machine-enforce CONVENTIONS.md #6 (no `unwrap`/`expect` outside tests) at low cost.
- Document the corrected clippy command (`--all-targets`) so local runs match CI.

**Non-Goals:**
- No delivery / publish / release / deploy stage (nowhere to deploy yet).
- No multi-OS or multi-toolchain matrix (toolchain is pinned to 1.96).
- Enforcing `missing_docs` (convention #4) — deferred to a follow-up change because it requires
  authoring ~48 contract doc-comments; sequenced after this mechanical work, same end-state.

## Decisions

- **`actions-rust-lang/setup-rust-toolchain@v1`** over `dtolnay/rust-toolchain` + manual cache.
  Rationale: it reads channel + components from `rust-toolchain.toml` (no version duplicated in
  YAML), bundles `Swatinem/rust-cache`, and its default `rustflags: -D warnings` gives the
  fail-on-warning requirement for free. Verified against the action README.
- **Single `ubuntu-latest` job, discrete steps** (`fmt` → `clippy --all-targets` → `build` →
  `test`). Rationale: a red check names the failing gate; caching makes recompiles cheap.
  Alternative (one combined script step) rejected — worse failure attribution.
- **No `protoc` install step.** `crates/proto/build.rs` uses `protoc-bin-vendored`.
- **Lint levels stay `warn` in `Cargo.toml`; CI denies via `-D warnings`.** Standard ratchet:
  local dev sees warnings (good DX), CI fails on them. Alternative (`deny` in Cargo.toml)
  rejected — makes local iteration painful.
- **`unwrap_used`/`expect_used` exemptions:** `#![cfg_attr(test, allow(...))]` on each crate
  root for unit tests; a plain `#![allow(...)]` on `crates/master/tests/integration.rs` because
  `cfg(test)` is **not** set for integration-test crates (a `cfg_attr(test, …)` there would
  silently do nothing). The 3 production `expect` sites get per-site `#[allow]` with a
  justification, which is itself convention-#6-compliant.
- **`config.rs` `<= 0` → `== 0`** is behavior-preserving: the field is unsigned, so `<= 0`
  already only matched `0`. This is a correctness-neutral fix, not lint-silencing.

## Risks / Trade-offs

- **Workflow YAML cannot be validated locally** → first push / PR is the real test; the apply
  phase iterates the workflow to green if the runner environment differs from local.
- **`actions-rust-lang/setup-rust-toolchain` default `RUSTFLAGS=-D warnings` is global** → also
  fails plain `cargo build`/`test` on rustc warnings. This is desired (strict policy); noted so
  it is not surprising.
- **Future code adding production `unwrap`/`expect`** will now fail CI → intended; that is the
  point of enforcing #6.
