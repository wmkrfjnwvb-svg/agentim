#!/bin/sh
# Builds the example app for Linux (agentim_client's build hook builds the
# Rust core with cargo and Flutter bundles it), then runs it against a real
# agentim under a virtual display.
set -e
cd "$(dirname "$0")"
root=$(cd ../../.. && pwd)
BIN="${AGENTIM_BIN:-$root/server/target/debug/agentim}"
flutter pub get >/dev/null && flutter build linux --release
app=build/linux/x64/release/bundle
ls "$app/lib" | grep -q libagentim_client.so
port=$((20800 + $(od -An -N1 -tu1 /dev/urandom)))
dir=$(mktemp -d)
"$BIN" --demo --db "$dir/a.db" --addr 127.0.0.1:$port > "$dir/server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null; rm -rf "$dir"' EXIT
for i in $(seq 100); do curl -sf http://127.0.0.1:$port/readyz > /dev/null && break; sleep 0.2; done
AGENTIM_SMOKE=1 AGENTIM_URL=http://127.0.0.1:$port timeout 120 xvfb-run -a "$app/agentim_example"
