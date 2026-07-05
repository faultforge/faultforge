#!/bin/sh
# E2E fixture plugin. `inject` writes a marker exactly like noop-marker, but
# `abort`/`cleanup` block far past any test deadline so the agent's dead-man
# backstop fires and the instance ends ERROR (scenario 3.5). Zero blast radius
# otherwise. Speaks the agent<->plugin NDJSON protocol (crates/fault).
set -eu

cmd="${1:-}"
input="$(cat)"

# Pull the first "key":"value" string out of the single-line JSON stdin input.
extract() {
  printf '%s' "$input" | sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"
}

iid="$(extract instance_id)"
marker="$(extract marker_path)"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

status() {
  printf '{"ts":"%s","instance_id":"%s","type":"status","state":"%s"}\n' "$ts" "$iid" "$1"
}

case "$cmd" in
  preflight)
    mkdir -p "$(dirname "$marker")"
    status PREFLIGHT
    ;;
  inject)
    status INJECTING
    mkdir -p "$(dirname "$marker")"
    printf '{"instance_id":"%s","plugin":"hang-cleanup@1"}' "$iid" > "$marker"
    status ACTIVE
    ;;
  report)
    if [ -f "$marker" ]; then status ACTIVE; else status DONE; fi
    ;;
  abort|cleanup)
    status RECOVERING
    sleep 3600
    ;;
  *)
    echo "usage: hang-cleanup <preflight|inject|report|abort|cleanup>" >&2
    exit 1
    ;;
esac
