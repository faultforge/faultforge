# FaultForge

> Chaos engineering for **bare-metal** hosts. *Break it before it breaks you.*

[![CI](https://github.com/faultforge/faultforge/actions/workflows/ci.yml/badge.svg)](https://github.com/faultforge/faultforge/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](rust-toolchain.toml)

FaultForge is a control plane for deliberately injecting failures into fleets of
bare-metal hosts, so you can find out how your systems break **on your terms** —
not at 3 a.m. in production.

A central **master** coordinates lightweight **agents** running on your target
hosts. Agents dial home over a single long-lived gRPC stream (NAT/firewall
friendly), and an operator drives the fleet from the `faultforge` CLI.

> [!NOTE]
> FaultForge is in early development. The current slice implements agent
> registration and heartbeating; fault injection is on the roadmap.

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

Requires a [Rust](https://rustup.rs) toolchain (pinned in `rust-toolchain.toml`).
No system `protoc` is needed — the protobuf compiler is vendored.

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

**3. Inspect the fleet** with the CLI:

```bash
# One-shot, scriptable
faultforge agents list
faultforge agents show <hostname>
faultforge -o table agents list      # human-readable output

# Or launch the interactive TUI (no subcommand, on a TTY)
faultforge
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Coding standards live in [CONVENTIONS.md](CONVENTIONS.md) and are mandatory for
contributions;

## License

Licensed under the [MIT License](LICENSE).
