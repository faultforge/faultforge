#!/usr/bin/env bash
# Scenario: start 2 agents, kill 1, verify via the faultforge CLI that only 1 is still
# heartbeating.
#
# The master + agents run in containers; the CLI runs straight from the host via `cargo run`
# (no dedicated container) and talks to the master's management API on localhost:8069.
#
# Note: killing an agent container stops its heartbeats but does NOT remove it from the master
# registry. Both agents remain listed; the killed one's last_seen_unix_ms stops advancing.
# A fresh last_seen_secs_ago < ~3 means the agent is actively heartbeating.
#
# Set SKIP_BUILD=1 to skip image rebuild.
# Run from the repository root.
set -euo pipefail

SKIP_BUILD=${SKIP_BUILD:-0}
NETWORK=faultforge-net
MASTER=faultforge-master
MGMT_URL=http://localhost:8069
HEARTBEAT_INTERVAL_SECS=5  # must match FAULTFORGE_HEARTBEAT_INTERVAL_SECS (master default)

check_cmd() {
    if ! command -v "$1" &>/dev/null; then
        echo "Error: '$1' is required but not installed." >&2
        exit 1
    fi
}
check_cmd podman
check_cmd cargo
check_cmd jq

cleanup() {
    echo ""
    echo "Cleaning up..."
    podman rm -f faultforge-agent-1 faultforge-agent-2 2>/dev/null || true
    podman rm -f "$MASTER" 2>/dev/null || true
    podman network rm "$NETWORK" 2>/dev/null || true
}
trap cleanup EXIT

# Remove any leftover containers from a previous interrupted run.
podman rm -f faultforge-agent-1 faultforge-agent-2 "$MASTER" 2>/dev/null || true
podman network rm "$NETWORK" 2>/dev/null || true

if [[ "$SKIP_BUILD" != "1" ]]; then
    echo "Building images..."
    podman build --target master -t faultforge-master .
    podman build --target agent  -t faultforge-agent  .
fi

# Build the CLI once up front so its compile output doesn't interleave with the test below.
echo "Building faultforge CLI..."
cargo build -q -p faultforge-cli

podman network create "$NETWORK"

# Master must bind to 0.0.0.0 — the default 127.0.0.1 is loopback-only inside a container.
podman run -d \
    --name "$MASTER" \
    --network "$NETWORK" \
    -p 8069:8069 \
    -e FAULTFORGE_LISTEN_ADDR=0.0.0.0:50051 \
    -e FAULTFORGE_MANAGEMENT_LISTEN_ADDR=0.0.0.0:8069 \
    faultforge-master

echo "Waiting 1s for master to start..."
sleep 1

podman run -d \
    --name faultforge-agent-1 \
    --hostname agent-1 \
    --network "$NETWORK" \
    -e FAULTFORGE_MASTER_ADDR=faultforge-master:50051 \
    faultforge-agent

podman run -d \
    --name faultforge-agent-2 \
    --hostname agent-2 \
    --network "$NETWORK" \
    -e FAULTFORGE_MASTER_ADDR=faultforge-master:50051 \
    faultforge-agent

WAIT_SECS=3
echo "Waiting ${WAIT_SECS}s for registration and first heartbeat..."
sleep "$WAIT_SECS"

show_agents() {
    # Annotate each agent with how many seconds ago its last heartbeat was received.
    local now_ms
    now_ms=$(( $(date +%s) * 1000 ))
    cargo run -q -p faultforge-cli -- --master-url "$MGMT_URL" agents list --output json \
        | jq --argjson now "$now_ms" \
            '[.[] | {hostname, last_seen_secs_ago: (($now - .last_seen_unix_ms) / 1000 | floor)}]'
}

echo ""
echo "=== Baseline: both agents running ==="
show_agents

echo ""
echo "Stopping agent-1..."
podman stop faultforge-agent-1

WAIT_SECS=$(( HEARTBEAT_INTERVAL_SECS + 2 ))
echo "Waiting ${WAIT_SECS}s (one heartbeat interval + 2s buffer)..."
sleep "$WAIT_SECS"

echo ""
echo "=== After kill: agent-1 should be stale, agent-2 still fresh ==="
show_agents

echo ""
echo "Test complete."
