# FaultForge

> Chaos engineering for **bare-metal** hosts. *Break it before it breaks you.*

[![CI](https://github.com/faultforge/faultforge/actions/workflows/ci.yml/badge.svg)](https://github.com/faultforge/faultforge/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)

FaultForge lets you inject failures into your bare-metal hosts on purpose, so you
learn how your systems break **on your terms** — not at 3 a.m. in production.

A central **master** controls small **agents** that run on your hosts. Each agent
dials the master and keeps one long-lived gRPC stream open (so it works through
NAT and firewalls). You drive the whole fleet from the `faultforge` CLI.

> [!NOTE]
> FaultForge is still early. Agents register, heartbeat, and can execute fault
> plugins with a safe, recoverable lifecycle (journal, safety timers, host
> quarantine) — but the master cannot dispatch faults yet, so there is no
> end-to-end fault injection from the CLI. That arrives with the master's
> dispatch slice.

## Architecture

```
  ┌────────────┐   gRPC bidi stream    ┌──────────────────┐
  │   agent    │ ────────────────────▶ │                  │
  │ (host A)   │ ◀──────────────────── │      master      │   HTTP    ┌─────────┐
  └────────────┘   register/heartbeat  │  (control plane) │ ◀──────── │   CLI   │
  ┌────────────┐                       │                  │  mgmt API │ (TUI +  │
  │   agent    │ ────────────────────▶ │   host registry  │ ────────▶ │ scripts)│
  │ (host B)   │ ◀──────────────────── │                  │           └─────────┘
  └────────────┘                       └──────────────────┘
```

| Crate | Binary | Role |
|-------|--------|------|
| `crates/proto`  | —                   | Shared gRPC contract (`faultforge.proto`) |
| `crates/master` | `faultforge-master` | gRPC server, host registry, HTTP management API |
| `crates/agent`  | `faultforge-agent`  | Runs on a target host; registers and heartbeats |
| `crates/cli`    | `faultforge`        | Operator CLI — one-shot scripting and an interactive TUI |

## Quick start

You need a [Rust](https://rustup.rs) toolchain (the version is pinned in
`rust-toolchain.toml`). You do **not** need to install `protoc` — it ships with
the project.

```bash
cargo build --workspace
```

**1. Start the master** (gRPC on `:50051`, management API on `:8069`):

```bash
cargo run -p faultforge-master
```

**2. Register an agent** on a target host:

```bash
cargo run -p faultforge-agent -- --master-addr http://master-host:50051
```

**3. Look at the fleet** with the CLI:

```bash
# One-shot, scriptable
faultforge --master-url http://master-host:8069 agents list
faultforge --master-url http://master-host:8069 agents show <hostname>
faultforge --master-url http://master-host:8069 -o table agents list      # human-readable output

# Or launch the interactive TUI (no subcommand, on a TTY)
faultforge
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Please follow the coding standards in [CONVENTIONS.md](CONVENTIONS.md).

## License

Licensed under the [MIT License](LICENSE).
