#!/bin/sh
# Builds the Android library (AAR): the Rust runtime and the JNI bridge for
# arm64-v8a, armeabi-v7a, x86_64 and x86 with 16 KB pages (Google Play,
# Android 15+), and the Kotlin wrapper compiled against android.jar. Then
# checks every library: 16 KB aligned, JNI entry points exported.
#
#   ANDROID_NDK=…/android-ndk-r28c ANDROID_JAR=…/android-35/android.jar KOTLINC=… sh android/build-aar.sh
set -e
cd "$(dirname "$0")/.."
root=$(cd ../.. && pwd)
API=24
NDK_BIN="$ANDROID_NDK/toolchains/llvm/prebuilt/linux-x86_64/bin"
out=build/aar
rm -rf "$out" && mkdir -p "$out/jni" build/android-classes
mkdir -p libs
fetch() { [ -f "libs/$2" ] || curl -sSfL -o "libs/$2" "https://repo1.maven.org/maven2/$1/$2"; }
fetch org/jetbrains/kotlinx/kotlinx-coroutines-core-jvm/1.10.2 kotlinx-coroutines-core-jvm-1.10.2.jar
PAGE="-Wl,-z,max-page-size=16384"
sh "$root/sdk/client-rust/native/build-android.sh" "$out/jni"
for pair in arm64-v8a:aarch64-linux-android armeabi-v7a:armv7a-linux-androideabi x86_64:x86_64-linux-android x86:i686-linux-android; do
  abi=${pair%%:*}; cc="$NDK_BIN/${pair#*:}$API-clang"
  "$cc" -shared -fPIC -O2 -Wall -Wextra -Werror $PAGE -o "$out/jni/$abi/libagentim_jni.so" src/main/c/agentim_jni.c \
    -I"$root/sdk/client-rust/native/include" -L"$out/jni/$abi" -lagentim_client -llog
  "$NDK_BIN/llvm-strip" --strip-unneeded "$out/jni/$abi/libagentim_jni.so"
done
"${KOTLINC:-kotlinc}" -Werror -no-jdk -jvm-target 11 -cp "$ANDROID_JAR:libs/kotlinx-coroutines-core-jvm-1.10.2.jar" -d build/android-classes src/main/kotlin 2>&1 | grep -v '^warning: unable to find kotlin-stdlib' || true
[ -f build/android-classes/com/agentim/client/AgentIMClient.class ]
(cd build/android-classes && jar cf ../aar/classes.jar com)
cp android/AndroidManifest.xml "$out/"
cp android/proguard.txt "$out/proguard.txt"
: > "$out/R.txt"
(cd "$out" && rm -f ../agentim-client.aar && jar cf ../agentim-client.aar AndroidManifest.xml classes.jar R.txt proguard.txt jni)
# Checks: every LOAD segment 16 KB aligned; the JNI bridge exports its entry points.
for so in "$out"/jni/*/*.so; do
  bad=$("$NDK_BIN/llvm-readelf" -lW "$so" | awk '$1=="LOAD" && $NF!="0x4000" {print}')
  [ -z "$bad" ] || { echo "not 16 KB aligned: $so"; echo "$bad"; exit 1; }
done
for so in "$out"/jni/*/libagentim_jni.so; do
  n=$("$NDK_BIN/llvm-nm" -D --defined-only "$so" | grep -c ' Java_com_agentim_client_Native_')
  [ "$n" -eq 7 ] || { echo "$so exports $n JNI functions, not 7"; exit 1; }
done
ls -l build/agentim-client.aar
for so in "$out"/jni/*/libagentim_client.so; do printf '%s %s bytes, %s gzipped\n' "$so" "$(wc -c < "$so")" "$(gzip -9 -c "$so" | wc -c)"; done
