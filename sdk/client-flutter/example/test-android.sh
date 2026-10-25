#!/bin/sh
# The example app on Android the way an app gets agentim_client (docs/12
# §2.12.1): the package as published (hook/prebuilt.json, here pointing at
# the given libraries by file://), a release APK for x86_64, run in the
# attached emulator against a real agentim (adb reverse).
#
#   AGENTIM_BIN=… sh test-android.sh <dir of agentim_client-android-<arch>.so>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
pkg=$(cd "$here/.." && pwd)
libs=$(cd "$1" && pwd)
work=$(mktemp -d)
port=$((21100 + $(od -An -N1 -tu1 /dev/urandom)))
mkdir "$work/agentim_client"
cp -R "$pkg/pubspec.yaml" "$pkg/lib" "$pkg/hook" "$work/agentim_client/"
rm -f "$work/agentim_client/hook/prebuilt.json"
sh "$pkg/prebuilt.sh" "$libs" "file://$libs/" "$work/agentim_client/hook/prebuilt.json" > /dev/null
cp -R "$here" "$work/example"
cd "$work/example"
rm -rf build .dart_tool
sed -i.bak 's#path: \.\.#path: ../agentim_client#' pubspec.yaml
flutter create --platforms android --project-name agentim_example . > /dev/null
python3 "$pkg/../allow-cleartext.py" android/app/src/main/AndroidManifest.xml
flutter build apk --release --target-platform android-x64 --dart-define=AGENTIM_URL=http://127.0.0.1:$port
apk=build/app/outputs/flutter-apk/app-release.apk
unzip -l "$apk" | grep -q "lib/x86_64/libagentim_client.so" || { echo "FAIL the core is not in the APK"; exit 1; }
"$AGENTIM_BIN" --demo --db "$work/a.db" --addr 127.0.0.1:$port > "$work/server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null' EXIT
for i in $(seq 100); do curl -sf http://127.0.0.1:$port/readyz > /dev/null && break; sleep 0.2; done
adb wait-for-device
adb reverse tcp:$port tcp:$port
adb install -r "$apk" > /dev/null
adb logcat -c
adb logcat -s flutter:* > "$work/app.log" 2>&1 &
logger=$!
adb shell am start -n com.example.agentim_example/.MainActivity > /dev/null
for i in $(seq 120); do grep -q "AGENTIM_SMOKE_" "$work/app.log" && break; sleep 1; done
kill $logger 2>/dev/null || true
cat "$work/app.log"
grep -q "AGENTIM_SMOKE_OK" "$work/app.log"
echo "ok   the Flutter app talks through the core in the Android emulator"
