#!/bin/sh
# The package as published (docs/12 §2.12.1): a copy outside the repository
# with hook/prebuilt.json, so the build hook downloads the core and checks
# it. The scenario runs against a real agentim; a wrong checksum fails the
# build.
#
#   sh test-package.sh <libagentim_client.so|.dylib|.dll for this machine>
set -e
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
DART="${DART:-dart}"
export AGENTIM_BIN="${AGENTIM_BIN:-$root/server/target/debug/agentim}"
lib=$1
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) key=linux-x64 ;; Linux-aarch64) key=linux-arm64 ;;
  Darwin-arm64) key=macos-arm64 ;; Darwin-x86_64) key=macos-x64 ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) key=windows-x64 ;;
  *) echo "unsupported: $(uname -s)-$(uname -m)" >&2; exit 2 ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/release" "$work/agentim_client"
cp "$lib" "$work/release/agentim_client-$key.${lib##*.}"
cp -R "$here/pubspec.yaml" "$here/lib" "$here/hook" "$here/test" "$work/agentim_client/"
rm -f "$work/agentim_client/hook/prebuilt.json"
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) base="file:///$(cygpath -m "$work/release")/" ;; *) base="file://$work/release/" ;; esac
sh "$here/prebuilt.sh" "$work/release" "$base" "$work/agentim_client/hook/prebuilt.json" >/dev/null
cd "$work/agentim_client"
"$DART" pub get >/dev/null
"$DART" run test/conformance.dart

# A library that does not match the recorded checksum is refused.
sed 's/"sha256": "[0-9a-f]\{8\}/"sha256": "00000000/' hook/prebuilt.json > hook/p && mv hook/p hook/prebuilt.json
rm -rf .dart_tool/hooks_runner
if "$DART" run test/conformance.dart > "$work/out" 2>&1; then echo "FAIL a wrong checksum was accepted"; exit 1; fi
grep -q "expected 00000000" "$work/out" || { cat "$work/out"; echo "FAIL not refused for its checksum"; exit 1; }
echo "ok   a wrong checksum fails the build"
