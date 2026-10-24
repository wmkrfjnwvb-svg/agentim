#!/bin/sh
# Builds the FFI shim and runs the conformance scenario against a real
# `agentim` with the Dart VM (Flutter packaging is in README.md).
set -e
cd "$(dirname "$0")"
root=$(cd ../.. && pwd)
lib="${AGENTIM_LIB:-$root/sdk/client-rust/target/debug}"
export AGENTIM_BIN="${AGENTIM_BIN:-$root/server/target/debug/agentim}"
DART="${DART:-dart}"
mkdir -p build
cc -shared -fPIC -O2 -Wall -Wextra -Werror -o build/libagentim_dart.so src/agentim_dart.c \
  -I"$root/sdk/client-rust/native/include" -L"$lib" -lagentim_client -Wl,-rpath,"$lib"
cp "$lib/libagentim_client.so" build/
"$DART" pub get >/dev/null
"$DART" format --output=none --set-exit-if-changed --line-length 160 lib test
"$DART" analyze --fatal-infos lib test
AGENTIM_LIB="$PWD/build" "$DART" run test/conformance.dart
