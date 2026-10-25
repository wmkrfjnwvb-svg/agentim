#!/bin/sh
# The native core for iOS as a dynamic XCFramework of frameworks (docs/12
# §2.12): AgentIMCore.framework for devices (arm64) and the simulator
# (arm64 + x86_64), each with its Info.plist and privacy manifest. React
# Native embeds it in the app (it is small; the static XCFramework is for
# SwiftPM). Uses the dylibs build-xcframework.sh built.
#
#   sh build-ios-frameworks.sh <out dir>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
ws=$(cd "$here/.." && pwd)
out=$(mkdir -p "$1" && cd "$1" && pwd)
v=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ws/Cargo.toml" | head -1)
lib() { echo "$ws/target/$1/release/libagentim_client.dylib"; }
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
framework() { # dir, binary inputs...
  d="$stage/$1/AgentIMCore.framework"; shift
  mkdir -p "$d"
  lipo -create "$@" -output "$d/AgentIMCore"
  install_name_tool -id @rpath/AgentIMCore.framework/AgentIMCore "$d/AgentIMCore"
  cp "$here/PrivacyInfo.xcprivacy" "$d/"
  cat >"$d/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>AgentIMCore</string>
  <key>CFBundleIdentifier</key><string>com.agentim.core</string>
  <key>CFBundleName</key><string>AgentIMCore</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>$v</string>
  <key>CFBundleVersion</key><string>$v</string>
  <key>MinimumOSVersion</key><string>17.0</string>
</dict>
</plist>
PLIST
}
framework device "$(lib aarch64-apple-ios)"
framework sim "$(lib aarch64-apple-ios-sim)" "$(lib x86_64-apple-ios)"
rm -rf "$out/AgentIMCore.xcframework"
xcodebuild -create-xcframework \
  -framework "$stage/device/AgentIMCore.framework" \
  -framework "$stage/sim/AgentIMCore.framework" \
  -output "$out/AgentIMCore.xcframework"
du -sh "$out/AgentIMCore.xcframework"
