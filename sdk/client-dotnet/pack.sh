#!/bin/sh
# Packs AgentIM.Client with the native core for every platform
# (docs/12 §2.12.1). <natives> holds one folder per .NET runtime id with the
# library inside: linux-x64/libagentim_client.so, osx-arm64/libagentim_client.dylib,
# win-x64/agentim_client.dll, ...
#
#   sh pack.sh <natives> <out dir>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
natives=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
rt="$here/AgentIM.Client/runtimes"
rm -rf "$rt"
for d in "$natives"/*/; do
  rid=$(basename "$d")
  mkdir -p "$rt/$rid/native"
  cp "$d"/*agentim_client* "$rt/$rid/native/"
done
dotnet pack "$here/AgentIM.Client" -c Release -o "$out"
# Every platform's library is in the package.
pkg=$(ls "$out"/AgentIM.Client.*.nupkg | head -1)
listing=$(python3 -c 'import sys, zipfile; print("\n".join(zipfile.ZipFile(sys.argv[1]).namelist()))' "$pkg")
echo "$listing"
for d in "$natives"/*/; do
  rid=$(basename "$d")
  echo "$listing" | grep -q "^runtimes/$rid/native/" || { echo "pack: $rid is missing" >&2; exit 1; }
done
