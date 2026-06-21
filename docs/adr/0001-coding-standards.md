# ADR-0001: Coding Standards

**Date:** 2026-06-21
**Status:** Accepted

---

## Context

FaultForge is a Rust workspace targeting bare-metal deployments. The codebase will grow across
multiple crates (proto, master, agent, cli) contributed to over time. Without explicit standards,
the usual drift occurs: untestable time-dependent logic, error types that leak across abstraction
boundaries, stringly-typed IDs, and comments that restate the obvious. Rust's type system and
tooling give us cheap enforcement — we should use it.

---

## Decision

### 1. Functional core / imperative shell

Pure business logic (parsing, state transitions, validation) lives in functions that take plain
data and return plain data. Side-effecting code (I/O, networking, clock reads) is pushed to the
outer shell. This makes core logic unit-testable without mocks.

### 2. Time as data; `Clock` trait only at the IO shell

Pass `now: SystemTime` into functions that need the current time. Do not call
`SystemTime::now()` inside library functions. A `Clock` trait (or equivalent injection point)
is acceptable only at the imperative shell where I/O already lives. This keeps tests
deterministic without introducing trait objects into every call site.

### 3. `SystemTime`, not `Instant`, for observable time

`last_seen` fields in the registry and `server_time_unix_ms` on the wire both require
unix-epoch milliseconds. `Instant` is opaque, monotonic, and cannot be serialized to an epoch
value. `SystemTime` is the right type for anything that crosses a process or network boundary.

### 4. Typed errors: `thiserror` in libraries, `anyhow` at binary boundaries

Library crates (`proto`, `master` lib, `agent` lib) define explicit error enums via `thiserror`.
Callers can match on variants. Binary entry points (`main.rs`) may use `anyhow` for
context-enriched propagation — they never need to match on error variants, only display them.
Mixed use (e.g. `anyhow` inside a lib's public API) is rejected in code review.

### 5. Newtypes over primitives

Domain identifiers are wrapped: `Hostname(String)`, not `String`. This prevents accidentally
passing a listen address where a hostname is expected. The newtype derives `Display`, `From`,
and `AsRef<str>` as needed. Raw primitives are only acceptable below the parsing boundary (e.g.
inside proto-generated structs).

### 6. Comments explain WHY, not WHAT

`///` doc comments are required on all public items and must document the contract (preconditions,
error conditions, notable invariants). `//` inline comments appear only where the code is not
self-explanatory — to record the reason for a non-obvious choice, not to translate Rust into
English. Comments that restate what the code does are removed.

### 7. Short, single-purpose functions

Each function has one responsibility and fits on a screen (~50 lines). Functions that grow beyond
this are a signal to decompose. This applies to async handlers too: extract named helpers rather
than nesting logic inside `tokio::spawn` closures.

### 8. Pedantic clippy lints via `[workspace.lints]`

Pedantic and restriction lints are configured at the workspace level in `Cargo.toml` under
`[workspace.lints]`. CI enforces `cargo clippy -- -D warnings`. This is not optional per-crate
override territory — all crates inherit the same lint profile.

---

## Consequences

- Time-dependent logic (e.g. staleness checks) is trivially unit-testable by passing a fixed
  `SystemTime`.
- Error types at crate boundaries are explicit; callers are not forced to accept `Box<dyn Error>`.
- Newtypes add a small boilerplate cost at parsing/serialization boundaries; this is intentional.
- The functional-core rule means side effects are concentrated and auditable; it does not mean
  zero `async` in lib code — async I/O wrappers are fine, but they must not embed pure logic.
- The comment discipline means new contributors must think before writing `// send the message`
  above a `tx.send(...)` call.
