#!/bin/sh
# Builds the native core for Android (docs/12 §2.12.1): arm64-v8a,
# armeabi-v7a, x86_64 and x86 with 16 KB pages (Google Play, Android 15+),
# stripped, into <out>/<abi>/libagentim_client.so; checks the alignment.
# Used by the AAR (sdk/client-kotlin) and the Dart package's prebuilts.
#
#   ANDROID_NDK=…/android-ndk-r28c sh build-android.sh <out>
set -e
ws=$(cd "$(dirname "$0")/.." && pwd)
out=$1
API=24
NDK_BIN="$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
PAGE="-Wl,-z,max-page-size=16384"
for pair in aarch64-linux-android:arm64-v8a:aarch64-linux-android armv7-linux-androideabi:armeabi-v7a:armv7a-linux-androideabi x86_64-linux-android:x86_64:x86_64-linux-android i686-linux-android:x86:i686-linux-android; do
  target=${pair%%:*}; rest=${pair#*:}; abi=${rest%%:*}; clang=${rest#*:}
  cc="$NDK_BIN/$clang$API-clang"
  t=$(echo "$target" | tr 'a-z-' 'A-Z_')
  env "CARGO_TARGET_${t}_LINKER=$cc" "CC_$(echo "$target" | tr '-' '_')=$cc" "AR_$(echo "$target" | tr '-' '_')=$NDK_BIN/llvm-ar" \
    "CARGO_TARGET_${t}_RUSTFLAGS=-C link-arg=$PAGE" \
    cargo build --manifest-path "$ws/Cargo.toml" -p agentim-client --release --locked --target "$target" --quiet
  mkdir -p "$out/$abi"
  cp "$ws/target/$target/release/libagentim_client.so" "$out/$abi/"
  "$NDK_BIN/llvm-strip" --strip-unneeded "$out/$abi/libagentim_client.so"
  bad=$("$NDK_BIN/llvm-readelf" -lW "$out/$abi/libagentim_client.so" | awk '$1=="LOAD" && $NF!="0x4000" {print}')
  [ -z "$bad" ] || { echo "not 16 KB aligned: $abi"; echo "$bad"; exit 1; }
done
