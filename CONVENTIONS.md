# FaultForge Coding Conventions

Quick reference for contributors. Each rule has a snippet — open this doc when writing code.
For the rationale behind each decision see the [See also](#see-also) footer.

---

## 1. Imperative shell, functional core

Decision logic is pure (`state + event + now -> (new state, effect)`). Mutation and I/O live at
the edge, not inside the core.

```rust
// Good — pure function, testable without any runtime
fn check_stale(entry: &ConnInfo, now: SystemTime) -> bool {
    now.duration_since(entry.last_seen)
        .map_or(false, |d| d > STALE_THRESHOLD)
}

// Bad — I/O buried inside logic; impossible to unit-test deterministically
fn check_stale(entry: &ConnInfo) -> bool {
    SystemTime::now()
        .duration_since(entry.last_seen)
        .map_or(false, |d| d > STALE_THRESHOLD)
}
```

---

## 2. Time is data

Pure functions receive `now: SystemTime` as a parameter. Call `SystemTime::now()` only inside
the IO shell (e.g. in a `tokio::spawn` task or `main`). Tests pass a fixed value.
A `Clock` trait is reserved for the IO shell where code genuinely must read the current time;
tests inject a fake one.

```rust
// Good — caller supplies the time; test passes a fixed value
fn register(registry: &mut Registry, hostname: Hostname, now: SystemTime) {
    registry.insert(hostname, ConnInfo { last_seen: now, .. });
}

// Bad — hidden dependency on wall clock; tests are time-sensitive
fn register(registry: &mut Registry, hostname: Hostname) {
    registry.insert(hostname, ConnInfo { last_seen: SystemTime::now(), .. });
}
```

Use `SystemTime`, not `Instant`, for anything observable outside the process (wire timestamps,
stored timestamps). `Instant` is opaque and cannot be serialized to a unix epoch.

---

## 3. Purity by default

Separate "compute what should happen" from "do it". The pure core may not be `async`; async
belongs only to I/O wrappers.

```rust
// Good — pure sync function; called by the async shell, not async itself
fn next_state(state: AgentState, event: Event) -> (AgentState, Option<Effect>) { ... }

// Bad — async crept in because one call site needed it; now the whole
//       state machine is infected and can't be tested without a runtime
async fn next_state(state: AgentState, event: Event) -> AgentState {
    self.db.load_flags().await?; // pulled into pure logic; now untestable
    ...
}
```

---

## 4. Comments explain WHY, never WHAT

Code must be self-explanatory: clear names and small functions carry the *what*. A comment earns
its place **only** when it does one of three things the code cannot:

1. explains **why** — the rationale, trade-off, or decision behind the code;
2. adds **context** the code cannot show — a reference to a spec/ADR/decision, an invariant a caller
   must uphold, a non-obvious consequence;
3. flags a **hack or gotcha** — a workaround, a footgun, why an error is deliberately ignored.

Anything that merely narrates the next line(s) — "send the register frame", "loop over the items",
"read the config" — is noise: **delete it and let the code speak.** If the code isn't clear enough
to stand alone, fix the code (rename, restructure), don't annotate it.

`///` doc-comments are **required** on all public items and must describe the contract
(preconditions, error conditions, invariants) — these are API documentation, not narration, and are
kept even when short. Inline `//` follows the three-reason test above.

```rust
// Good — the doc states the contract; the inline note gives a reason the code can't
/// Returns `true` if the agent has not sent a heartbeat within `STALE_THRESHOLD`.
/// `now` must be monotonically non-decreasing across calls for the result to be meaningful.
fn is_stale(entry: &ConnInfo, now: SystemTime) -> bool {
    // Saturating: a clock that jumped backwards must not read as "fresh".
    now.duration_since(entry.last_seen).map_or(true, |d| d > STALE_THRESHOLD)
}

// Bad — restates the code, says nothing the code doesn't
// Check if the entry is stale
fn is_stale(entry: &ConnInfo, now: SystemTime) -> bool { ... }
```

---

## 5. Short, single-purpose functions

One responsibility per function. ~50 lines is a signal to review decomposition — not a hard
limit. The function name should say exactly what it does.

```rust
// Good — each function has one job
fn parse_config(raw: RawConfig) -> Result<Config, ConfigError> { ... }
fn validate_hostname(s: &str) -> Result<Hostname, HostnameError> { ... }

// Bad — parses, validates, and emits metrics in one blob
fn handle_registration(raw: RawConfig, s: &str, metrics: &Metrics) -> Result<...> {
    // 80 lines doing several unrelated things
}
```

---

## 6. Typed errors — and no `unwrap`/`expect` outside tests

> **CI-enforced:** `clippy::unwrap_used` and `clippy::expect_used` are enabled workspace-wide.
> Test code is exempt via `#![cfg_attr(test, allow(...))]`. Production `expect` sites must
> carry a per-site `#[allow(clippy::expect_used)]` with a comment explaining why the panic is
> impossible.



`thiserror` in library crates; `anyhow` only at binary entry points (`main` / `run_*`). No
`unwrap()` or `expect()` outside tests and provably-impossible cases — when `expect` is used,
the message must say *why* the panic is impossible.

```rust
// Good — explicit enum in lib; anyhow only in main
// lib.rs
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("hostname already registered: {0}")]
    Duplicate(Hostname),
}

// main.rs
fn main() -> anyhow::Result<()> { ... }

// Bad — opaque error escapes library boundary
pub fn register(...) -> Result<(), Box<dyn std::error::Error>> { ... }
```

```rust
// Bad — unexplained unwrap
let hostname = parts.next().unwrap();

// Good — impossible case with a reason
let hostname = parts.next().expect("split always yields at least one element");
```

---

## 7. Newtypes over primitives

Wrap domain identifiers: `Hostname(String)`, not `String`. Parse and validate once at the
boundary; trust the type everywhere inside.

```rust
// Good
pub struct Hostname(String);

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HostnameError {
    #[error("hostname must not be empty")]
    Empty,
}

impl Hostname {
    /// Parses and validates a hostname string.
    pub fn parse(s: &str) -> Result<Self, HostnameError> { ... }
}

fn register(hostname: Hostname) { ... } // can't accidentally pass a listen addr

// Bad — stringly typed; wrong value compiles fine
fn register(hostname: String) { ... }
```

---

## 8. Immutability by default

Declare variables without `mut` unless mutation is required. Prefer returning new values over
mutating in place.

```rust
// Good
let config = load_config(args)?;

// Bad — unnecessary mut, signals possible later mutation that doesn't exist
let mut config = load_config(args)?;
```

---

## 9. Module layout

Each binary's `lib.rs` is the entry point to a layered module tree:

```
lib.rs        — re-exports + doc comment
config.rs     — layered config (file → env → flags)
<domain>.rs   — pure core (types, state machines, pure functions)
server.rs     — IO shell (gRPC/REST listener setup)
session.rs    — IO shell (per-connection async task)
```

One clear responsibility per file. Do not let pure domain types leak into `server.rs` or vice
versa.

```rust
// Good — IO shell calls into the pure registry; registry knows nothing about gRPC
// session.rs
pub async fn run(mut stream: SessionStream, registry: Arc<Mutex<Registry>>) {
    let event = stream.recv().await;
    registry.lock().unwrap().apply(event, SystemTime::now());
}

// Bad — gRPC stream handling mixed into the registry type; breaks the layer boundary
// registry.rs
impl Registry {
    pub async fn handle_stream(&self, stream: SessionStream) { ... }
}
```

---

## See also

[docs/adr/0001-coding-standards.md](docs/adr/0001-coding-standards.md) — the rationale and
trade-offs behind each convention above.
