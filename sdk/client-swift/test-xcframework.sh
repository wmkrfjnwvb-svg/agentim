#!/bin/sh
# Uses the Swift wrapper the way an app would, with the core from the
# XCFramework (docs/12 §2.12.1): a package with a binaryTarget, the
# conformance scenario run on macOS against a real server (built and
# linked), and the library compiled for the iOS simulator and an iOS device
# (a library is not linked until an app uses it).
#
#   AGENTIM_BIN=… sh test-xcframework.sh <path/to/AgentIMCore.xcframework>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
xc=$(cd "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/Sources"
cp -R "$here/Sources/AgentIM" "$here/Sources/Conformance" "$work/Sources/"
cp -R "$xc" "$work/AgentIMCore.xcframework"
cat >"$work/Package.swift" <<'EOF'
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "AgentIM",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [.library(name: "AgentIM", targets: ["AgentIM"])],
    targets: [
        .binaryTarget(name: "CAgentIM", path: "AgentIMCore.xcframework"),
        // The core uses the system's certificates (Security) and network
        // reachability; a static library brings no link settings itself.
        .target(
            name: "AgentIM",
            dependencies: ["CAgentIM"],
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("SystemConfiguration"),
            ]
        ),
        .executableTarget(name: "Conformance", dependencies: ["AgentIM"]),
    ]
)
EOF
cd "$work"
echo "== macOS: the conformance scenario"
swift build -Xswiftc -warnings-as-errors
.build/debug/Conformance
echo "== iOS simulator and device: the library compiles"
for dest in 'generic/platform=iOS Simulator' 'generic/platform=iOS'; do
  xcodebuild build -scheme AgentIM -destination "$dest" -derivedDataPath "$work/dd" \
    CODE_SIGNING_ALLOWED=NO -quiet
done
echo "XCFramework: all checks passed"
