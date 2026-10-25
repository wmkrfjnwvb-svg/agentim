#!/bin/sh
# Builds AgentIMCore.xcframework on macOS (docs/12 §2.12.1): the native core
# as a static library for iOS devices (arm64), the iOS simulator (arm64 and
# x86_64) and macOS (arm64 and x86_64), with the C header and a module map
# for the module `CAgentIM`. Also writes the zip SwiftPM downloads and its
# checksum (the SHA-256 of the zip, as `swift package compute-checksum`).
#
#   sh build-xcframework.sh [out dir]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
ws=$(cd "$here/.." && pwd)
out=${1:-$ws/target/xcframework}
mkdir -p "$out"
out=$(cd "$out" && pwd)
# The same minimum versions as the Swift package.
export IPHONEOS_DEPLOYMENT_TARGET=17.0 MACOSX_DEPLOYMENT_TARGET=14.0
for t in aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked -p agentim-client --target "$t" --manifest-path "$ws/Cargo.toml"
done
lib() { echo "$ws/target/$1/release/libagentim_client.a"; }
rm -rf "$out/AgentIMCore.xcframework" "$out/stage"
mkdir -p "$out/stage/sim" "$out/stage/mac" "$out/stage/headers"
lipo -create "$(lib aarch64-apple-ios-sim)" "$(lib x86_64-apple-ios)" -output "$out/stage/sim/libagentim_client.a"
lipo -create "$(lib aarch64-apple-darwin)" "$(lib x86_64-apple-darwin)" -output "$out/stage/mac/libagentim_client.a"
cp "$here/include/agentim.h" "$out/stage/headers/"
cat >"$out/stage/headers/module.modulemap" <<'EOF'
module CAgentIM {
    header "agentim.h"
    export *
}
EOF
xcodebuild -create-xcframework \
  -library "$(lib aarch64-apple-ios)" -headers "$out/stage/headers" \
  -library "$out/stage/sim/libagentim_client.a" -headers "$out/stage/headers" \
  -library "$out/stage/mac/libagentim_client.a" -headers "$out/stage/headers" \
  -output "$out/AgentIMCore.xcframework"
rm -rf "$out/stage"
cd "$out"
rm -f AgentIMCore.xcframework.zip
ditto -c -k --sequesterRsrc --keepParent AgentIMCore.xcframework AgentIMCore.xcframework.zip
shasum -a 256 AgentIMCore.xcframework.zip | awk '{print $1}' >AgentIMCore.xcframework.zip.checksum
ls -l AgentIMCore.xcframework.zip
cat AgentIMCore.xcframework.zip.checksum
