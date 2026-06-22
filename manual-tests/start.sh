#!/usr/bin/env bash
# Usage: ./manual-tests/start.sh [AGENT_COUNT]
# Starts a local master + N agents for manual exploration. Ctrl-C tears everything down.
# Set SKIP_BUILD=1 to skip image rebuild.
set -euo pipefail

AGENT_COUNT=${1:-2}
SKIP_BUILD=${SKIP_BUILD:-0}
NETWORK=faultforge-net
MASTER=faultforge-master

check_cmd() {
    if ! command -v "$1" &>/dev/null; then
        echo "Error: '$1' is required but not installed." >&2
        exit 1
    fi
}
check_cmd podman
check_cmd curl

cleanup() {
    echo ""
    echo "Tearing down..."
    for i in $(seq 1 "$AGENT_COUNT"); do
        podman rm -f "faultforge-agent-$i" 2>/dev/null || true
    done
    podman rm -f "$MASTER" 2>/dev/null || true
    podman network rm "$NETWORK" 2>/dev/null || true
}
trap cleanup EXIT

if [[ "$SKIP_BUILD" != "1" ]]; then
    echo "Building images..."
    podman build --target master -t faultforge-master .
    podman build --target agent  -t faultforge-agent  .
fi

podman network create "$NETWORK" 2>/dev/null || true

# Master must bind to 0.0.0.0 — the default 127.0.0.1 is loopback-only inside a container.
podman run -d \
    --name "$MASTER" \
    --network "$NETWORK" \
    -p 8069:8069 \
    -e FAULTFORGE_LISTEN_ADDR=0.0.0.0:50051 \
    -e FAULTFORGE_MANAGEMENT_LISTEN_ADDR=0.0.0.0:8069 \
    faultforge-master

echo "Waiting for master..."
sleep 1

for i in $(seq 1 "$AGENT_COUNT"); do
    podman run -d \
        --name "faultforge-agent-$i" \
        --hostname "agent-$i" \
        --network "$NETWORK" \
        -e FAULTFORGE_MASTER_ADDR=faultforge-master:50051 \
        faultforge-agent
    echo "Started agent-$i"
done

echo ""
echo "Cluster running:"
echo "  Master mgmt:  http://localhost:8069"
# shellcheck disable=SC2046
echo "  Agents:       $(seq 1 "$AGENT_COUNT" | sed 's/^/agent-/' | tr '\n' ' ')"
echo ""
echo "  curl -s http://localhost:8069/agents | jq ."
echo ""
echo "Press Ctrl-C to stop."

# `sleep infinity` is GNU-only; BSD/macOS sleep rejects it. Loop a finite sleep
# in the background and `wait` on it so Ctrl-C still interrupts promptly.
while true; do
    sleep 86400 & wait $!
done
