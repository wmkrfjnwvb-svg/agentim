#!/bin/sh
# The Unity package (docs/12 §2.12.1) as a UPM tarball for "Add package from
# tarball" (and the GitHub release): the same C# as the NuGet package, the
# WebGL bridge, and the native core per platform, each file with its .meta.
# On macOS (the macOS plugin is a universal dylib made with lipo).
#
#   sh pack.sh <native dir> <AAR jni dir> <iOS libagentim_client.a> <out dir>
#     <native dir>: osx-x64/, osx-arm64/ (dylib), win-x64/ (dll), as the
#     sdk-packages native job lays them out
set -eu
here=$(cd "$(dirname "$0")" && pwd)
native=$(cd "$1" && pwd) jni=$(cd "$2" && pwd) ios=$3 out=$(mkdir -p "$4" && cd "$4" && pwd)
v=$(sh "$here/../../version.sh")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg="$work/package"
mkdir -p "$pkg/Runtime/Plugins"
cp "$here/package.json" "$here/../../../LICENSE" "$pkg/"
mv "$pkg/LICENSE" "$pkg/LICENSE.md"
cp "$here/Runtime/AgentIM.Client.asmdef" "$here/Runtime/AgentIMRunner.cs" \
  "$here/../AgentIM.Client/Client.cs" "$here/../AgentIM.Client/Native.cs" "$pkg/Runtime/"
cp -R "$here/Runtime/Plugins/WebGL" "$pkg/Runtime/Plugins/"
for abi in arm64-v8a armeabi-v7a x86_64; do
  mkdir -p "$pkg/Runtime/Plugins/Android/$abi"
  cp "$jni/$abi/libagentim_client.so" "$pkg/Runtime/Plugins/Android/$abi/"
done
mkdir -p "$pkg/Runtime/Plugins/iOS" "$pkg/Runtime/Plugins/macOS" "$pkg/Runtime/Plugins/Windows/x86_64"
cp "$ios" "$pkg/Runtime/Plugins/iOS/libagentim_client.a"
lipo -create "$native/osx-x64/libagentim_client.dylib" "$native/osx-arm64/libagentim_client.dylib" \
  -output "$pkg/Runtime/Plugins/macOS/libagentim_client.dylib"
cp "$native/win-x64/agentim_client.dll" "$pkg/Runtime/Plugins/Windows/x86_64/"
python3 "$here/meta.py" "$pkg"
grep -q "\"version\": \"$v\"" "$pkg/package.json"
(cd "$work" && tar -czf "$out/com.agentim.client-$v.tgz" package)
ls -l "$out/com.agentim.client-$v.tgz"
