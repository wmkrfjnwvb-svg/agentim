#!/bin/sh
# Puts the native core into the package before `npm pack` (docs/12 §2.12):
# the Android libraries (from the AAR build: 4 ABIs, 16 KB pages), the iOS
# dynamic XCFramework and the C header. Not in git.
#
#   sh prepare-native.sh <jni dir with <abi>/libagentim_client.so> <AgentIMCore.xcframework>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
jni=$1 xc=$2
rm -rf "$here/android/jniLibs" "$here/ios/AgentIMCore.xcframework" "$here/include"
for abi in arm64-v8a armeabi-v7a x86_64 x86; do
  mkdir -p "$here/android/jniLibs/$abi"
  cp "$jni/$abi/libagentim_client.so" "$here/android/jniLibs/$abi/"
done
cp -R "$xc" "$here/ios/AgentIMCore.xcframework"
mkdir -p "$here/include"
cp "$here/../client-rust/native/include/agentim.h" "$here/include/"
du -sh "$here/android/jniLibs" "$here/ios/AgentIMCore.xcframework"
