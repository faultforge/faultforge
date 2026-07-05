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
> FaultForge is still early, but the core loop is closed: you can run a fault
> experiment from the CLI end to end — the master validates it against its
> plugin catalog, dispatches to agents, tracks every instance, halts on demand
> or on failure, and reports `COMPLETED` / `ABORTED` / `ERROR`. What's missing
> for production use: metric-based guardrails/verdicts, tag targeting, and —
> critically — authentication/TLS (keep both planes on a trusted network).

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
| `crates/fault`  | —                   | Shared fault contract: manifests, params, digests, plugin protocol |
| `crates/master` | `faultforge-master` | Control plane: registry, plugin catalog, experiment dispatch, HTTP management API |
| `crates/agent`  | `faultforge-agent`  | Runs on a target host; executes fault instances with a safe, recoverable lifecycle |
| `crates/cli`    | `faultforge`        | Operator CLI — one-shot scripting and an interactive TUI |
| `crates/plugins/noop-marker` | `noop-marker` | Reference fault plugin (zero blast radius) |
| `crates/e2e`    | — (tests)           | Dev-only end-to-end harness: runs the real binaries in rootless podman containers |

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

Both hosts need the plugin catalog on disk (the same directory layout for the
master's `--catalog-root` and the agent's `--plugin-root`, default
`/usr/lib/faultforge/plugins`): one `<name>@<version>/` directory per plugin
holding `manifest.yaml` and the executable.

**3. Look at the fleet** with the CLI:

```bash
# One-shot, scriptable
faultforge --master-url http://master-host:8069 agents list
faultforge --master-url http://master-host:8069 agents show <hostname>
faultforge --master-url http://master-host:8069 -o table agents list      # human-readable output

# Or launch the interactive TUI (no subcommand, on a TTY)
faultforge
```

**4. Run an experiment.** Write a definition:

```yaml
# exp.yaml
name: first-fault
actions:
  - hosts: [web-01]
    plugin: { name: noop-marker, version: "1" }
    params: { marker_path: /tmp/faultforge-marker }
    duration_secs: 30
```

and drive it:

```bash
faultforge experiment run -f exp.yaml --wait   # exit 0 COMPLETED, 4 ABORTED, 5 ERROR
faultforge experiment list
faultforge experiment show <id>
faultforge experiment halt <id>                # fire the kill-switch
faultforge agents clear-taint <hostname>       # lift a host quarantine
```

The master validates everything up front (catalog, params, durations, targets),
fans the faults out as one salvo, and stops **all** instances if any one of
them fails — a host that cannot be cleaned up is quarantined (`TAINTED`) until
an operator clears it.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Please follow the coding standards in [CONVENTIONS.md](CONVENTIONS.md).

### End-to-end tests (podman)

`crates/e2e` runs the **real** master, agent, and CLI as separate rootless-podman
containers and asserts the full fault lifecycle (journal replay after a real
agent crash, master-loss self-abort, dead-man backstop, taint quarantine) through
the operator surface and host ground truth. These scenarios are `#[ignore]` by
default, so the command above stays green on machines without podman. To run them:

```bash
# macOS: start the VM first (Linux with rootless podman needs no VM step)
podman machine start

cargo test -p faultforge-e2e -- --ignored
```

The suite builds its container images on first run (a few minutes; cached
afterwards). It is also wired to a **manual, non-blocking** `E2E` GitHub Actions
workflow (`workflow_dispatch`) — it does not gate pull requests. See
[`crates/e2e/README.md`](crates/e2e/README.md) for details.

## License

Licensed under the [MIT License](LICENSE).
