#!/usr/bin/env bash
# Runs the Node, Python and Go SDK smoke tests against a fresh server.
# Usage: sdk/smoke.sh [path/to/agentim]   (default: server/target/debug/agentim)
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${1:-server/target/debug/agentim}
DIR=$(mktemp -d)
PORT=${PORT:-18765}
"$BIN" --db "$DIR/smoke.db" --addr "127.0.0.1:$PORT" >"$DIR/server.log" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true; rm -rf "$DIR"' EXIT
for _ in $(seq 1 100); do
  curl -fs "http://127.0.0.1:$PORT/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done
SCOPES=tokens:revoke,agents:write,agents:read,users:write,users:read,conversations:write,conversations:read,messages:write,messages:read,compliance,moderation
KEY=$("$BIN" --db "$DIR/smoke.db" api-key create --name smoke --scopes "$SCOPES" 2>/dev/null | sed -n 's/.*"key": "\(ak_[^"]*\)".*/\1/p')
test -n "$KEY"
SK=$("$BIN" --db "$DIR/smoke.db" signing-key create --name smoke 2>/dev/null)
KID=$(echo "$SK" | sed -n 's/.*"kid": "\([^"]*\)".*/\1/p')
SECRET=$(echo "$SK" | sed -n 's/.*"secret": "\([^"]*\)".*/\1/p')
test -n "$KID" && test -n "$SECRET"
export AGENTIM_URL="http://127.0.0.1:$PORT" AGENTIM_KEY="$KEY" AGENTIM_KID="$KID" AGENTIM_SECRET="$SECRET"
node sdk/node/smoke.mjs
python3 sdk/python/smoke.py
(cd sdk/go && go run ./smoke)
