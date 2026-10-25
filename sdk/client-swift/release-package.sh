#!/bin/sh
# Writes the content of the `agentim-swift` repository (docs/06 D125): the
# Swift wrapper and a Package.swift at its root whose binaryTarget is the
# release's XCFramework, by URL and SwiftPM checksum. The release workflow
# commits this to that repository and tags it with the SDK version.
#
#   sh release-package.sh <out dir> <AgentIMCore.xcframework.zip URL> <checksum>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=$1 url=$2 sum=$3
case "$sum" in *[!0-9a-f]*|"") echo "checksum must be hex: $sum" >&2; exit 2 ;; esac
[ ${#sum} -eq 64 ] || { echo "checksum must be 64 hex digits" >&2; exit 2; }
rm -rf "$out" && mkdir -p "$out/Sources"
cp -R "$here/Sources/AgentIM" "$out/Sources/"
cp "$here/README.md" "$out/README.md"
cp "$here/../../LICENSE" "$out/LICENSE"
cat >"$out/Package.swift" <<SWIFT
// swift-tools-version:5.9
// AgentIM client SDK for iOS and macOS. Generated from
// sdk/client-swift in the AgentIM repository; do not edit here.
import PackageDescription

let package = Package(
    name: "AgentIM",
    platforms: [.macOS(.v14), .iOS(.v17)],
    products: [.library(name: "AgentIM", targets: ["AgentIM"])],
    targets: [
        .binaryTarget(
            name: "CAgentIM",
            url: "$url",
            checksum: "$sum"
        ),
        .target(
            name: "AgentIM",
            dependencies: ["CAgentIM"],
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("SystemConfiguration"),
            ]
        ),
    ]
)
SWIFT
echo "wrote $out"
