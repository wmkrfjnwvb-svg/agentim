#!/usr/bin/env bash
# End-to-end checks of the agent SDKs (Node, Python) against a server each
# script starts itself (docs/en/design/agents.md).
# Usage: sdk/agent_e2e.sh [path/to/agentim]   (default: server/target/debug/agentim)
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${1:-server/target/debug/agentim}
PY=${PYTHON:-python3}
node sdk/agent-node/files.test.mjs
"$PY" sdk/agent-python/files_test.py
AGENTIM_BIN="$BIN" node sdk/agent-node/e2e.mjs
AGENTIM_BIN="$BIN" "$PY" sdk/agent-python/e2e.py
