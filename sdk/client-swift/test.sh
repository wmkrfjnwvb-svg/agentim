#!/bin/sh
# Builds the wrapper against libagentim_client and runs the conformance
# scenario against a real `agentim` (Linux or macOS; iOS packaging is in README.md).
set -e
cd "$(dirname "$0")"
root=$(cd ../.. && pwd)
lib="${AGENTIM_LIB:-$root/sdk/client-rust/target/debug}"
export AGENTIM_BIN="${AGENTIM_BIN:-$root/server/target/debug/agentim}"
swift build -Xswiftc -warnings-as-errors -Xlinker -L"$lib" -Xlinker -rpath -Xlinker "$lib"
.build/debug/Conformance
