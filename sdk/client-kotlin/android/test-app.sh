#!/bin/sh
# The AAR in an Android app (docs/12 §2.12.1): android/test-app depends on
# the AAR file, is built with Gradle 9.7.1 (checked download) and run in
# the attached emulator against a real agentim (adb reverse).
#
#   AGENTIM_BIN=… sh android/test-app.sh <agentim-client.aar>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
aar=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
work=$(mktemp -d)
port=$((21300 + $(od -An -N1 -tu1 /dev/urandom)))
g=gradle-9.7.1
( cd "$work" && curl -sSfLO "https://services.gradle.org/distributions/$g-bin.zip" \
  && echo "$(curl -sSfL "https://services.gradle.org/distributions/$g-bin.zip.sha256")  $g-bin.zip" | sha256sum -c - > /dev/null \
  && unzip -q "$g-bin.zip" )
cp -R "$here/test-app" "$work/app"
mkdir -p "$work/app/app/libs" && cp "$aar" "$work/app/app/libs/agentim-client.aar"
(cd "$work/app" && "$work/$g/bin/gradle" --no-daemon -q assembleDebug -PagentimUrl=http://127.0.0.1:$port)
apk="$work/app/app/build/outputs/apk/debug/app-debug.apk"
for f in lib/x86_64/libagentim_client.so lib/x86_64/libagentim_jni.so; do
  unzip -l "$apk" | grep -q "$f" || { echo "FAIL $f is not in the APK"; exit 1; }
done
"$AGENTIM_BIN" --demo --db "$work/a.db" --addr 127.0.0.1:$port > "$work/server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
for i in $(seq 100); do curl -sf http://127.0.0.1:$port/readyz > /dev/null && break; sleep 0.2; done
adb wait-for-device
adb reverse tcp:$port tcp:$port
adb install -r "$apk" > /dev/null
adb logcat -c
adb logcat -s AgentIM:* > "$work/app.log" 2>&1 &
logger=$!
adb shell am start -n com.agentim.aartest/.MainActivity > /dev/null
for i in $(seq 120); do grep -q "AGENTIM_SMOKE" "$work/app.log" && break; sleep 1; done
kill $logger 2>/dev/null || true
cat "$work/app.log"
grep -q "AGENTIM_SMOKE ok" "$work/app.log"
echo "ok   two people talk through the AAR in the Android emulator"
