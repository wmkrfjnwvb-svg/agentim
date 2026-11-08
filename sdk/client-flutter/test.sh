#!/bin/sh
# Runs the conformance scenario against a real `agentim` with the Dart VM;
# the build hook builds the core from ../client-rust (Flutter: example/).
set -e
cd "$(dirname "$0")"
root=$(cd ../.. && pwd)
export AGENTIM_BIN="${AGENTIM_BIN:-$root/server/target/debug/agentim}"
DART="${DART:-dart}"
"$DART" pub get --no-example >/dev/null
"$DART" format --output=none --set-exit-if-changed --line-length 160 lib test hook
"$DART" analyze --fatal-infos lib test hook
"$DART" run test/conformance.dart
