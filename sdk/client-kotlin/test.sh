#!/bin/sh
# Builds the JNI bridge and the wrapper, then runs the conformance scenario
# against a real `agentim` (JVM; Android packaging is in README.md).
#
#   KOTLINC=…/kotlinc JAVA_HOME=… sh test.sh
set -e
cd "$(dirname "$0")"
root=$(cd ../.. && pwd)
lib="${AGENTIM_LIB:-$root/sdk/client-rust/target/debug}"
export AGENTIM_BIN="${AGENTIM_BIN:-$root/server/target/debug/agentim}"
JAVA_HOME="${JAVA_HOME:-$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")}"
KOTLINC="${KOTLINC:-kotlinc}"
mkdir -p build libs
fetch() { [ -f "libs/$2" ] || curl -sSfL -o "libs/$2" "https://repo1.maven.org/maven2/$1/$2"; }
fetch org/jetbrains/kotlinx/kotlinx-coroutines-core-jvm/1.10.2 kotlinx-coroutines-core-jvm-1.10.2.jar
fetch org/json/json/20250517 json-20250517.jar
cp=libs/kotlinx-coroutines-core-jvm-1.10.2.jar:libs/json-20250517.jar
cc -shared -fPIC -O2 -Wall -Wextra -Werror -o build/libagentim_jni.so src/main/c/agentim_jni.c \
  -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/linux" -I"$root/sdk/client-rust/native/include" \
  -L"$lib" -lagentim_client -Wl,-rpath,"$lib"
"$KOTLINC" -Werror -cp "$cp" -d build/classes src/main/kotlin src/test/kotlin 2>&1 | grep -v '^warning: unable to find kotlin-stdlib' || true
[ -f build/classes/ConformanceKt.class ]
java -Dagentim.jni="$PWD/build/libagentim_jni.so" -cp "build/classes:$cp:$(dirname "$(readlink -f "$KOTLINC")")/../lib/kotlin-stdlib.jar" ConformanceKt
