#!/bin/sh
# The client SDK version (docs/12 §2.12.1): every package carries the one in
# sdk/client-rust/Cargo.toml. Prints it after checking each manifest and
# each wrapper's default client_version; with a tag, also that the tag is
# sdk-v<version>. Exits 1 on any mismatch.
#
#   sh sdk/version.sh [sdk-v<version>]
set -eu
cd "$(dirname "$0")"
v=$(sed -n 's/^version = "\(.*\)"/\1/p' client-rust/Cargo.toml | head -1)
bad=0
check() { # file, the text that must be in it
  grep -qF -- "$2" "$1" || { echo "$1: expected $2" >&2; bad=1; }
}
for p in client-web react client-react-native client-dotnet/unity; do check "$p/package.json" "\"version\": \"$v\""; done
for p in react client-react-native; do check "$p/package.json" "\"@agentimhq/client-web\": \"${v%.*}.x\""; done
check client-dotnet/AgentIM.Client/AgentIM.Client.csproj "<Version>$v</Version>"
check client-flutter/pubspec.yaml "version: $v"
check client-dotnet/AgentIM.Client/Client.cs "ClientVersion { get; set; } = \"$v\""
check client-flutter/lib/agentim_client.dart "this.clientVersion = '$v'"
check client-kotlin/src/main/kotlin/com/agentim/client/AgentIMClient.kt "val clientVersion: String = \"$v\""
check client-react-native/src/index.ts "o.clientVersion ?? '$v'"
check client-swift/Sources/AgentIM/AgentIMClient.swift "public var clientVersion = \"$v\""
if [ $# -gt 0 ] && [ "$1" != "sdk-v$v" ]; then echo "tag $1 is not sdk-v$v" >&2; bad=1; fi
[ $bad = 0 ] || exit 1
echo "$v"
