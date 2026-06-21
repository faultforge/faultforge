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
// Good — compute effect, apply it outside
fn next_state(state: AgentState, event: Event) -> (AgentState, Option<Effect>) { ... }

// Bad — async computation mixed with pure logic
async fn next_state(state: AgentState, event: Event) -> AgentState {
    tokio::time::sleep(Duration::from_secs(1)).await; // why is this here?
    ...
}
```

---

## 4. Comments explain WHY

`///` doc-comments are required on all public items and must describe the contract (preconditions,
error conditions, invariants). Inline `//` only for non-obvious rationale. Never restate what
the code already says.

```rust
// Good
/// Returns `true` if the agent has not sent a heartbeat within `STALE_THRESHOLD`.
/// `now` must be monotonically non-decreasing across calls for the result to be meaningful.
fn is_stale(entry: &ConnInfo, now: SystemTime) -> bool { ... }

// Bad — restates the code, says nothing about the contract
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

## 6. Typed errors

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

impl Hostname {
    /// Parses and validates a hostname string.
    pub fn parse(s: impl Into<String>) -> Result<Self, HostnameError> { ... }
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
// Good — clean module boundary
// master/src/registry.rs — pure: HashMap logic, stale checks
// master/src/session.rs  — IO: reads stream, calls registry functions

// Bad — gRPC stream handling mixed into the registry type
impl Registry {
    pub async fn handle_stream(&self, stream: SessionStream) { ... }
}
```

---

## See also

[docs/adr/0001-coding-standards.md](docs/adr/0001-coding-standards.md) — the rationale and
trade-offs behind each convention above.
