#!/bin/sh
# Builds a new React Native 0.87.1 app with the package installed from its
# npm tarball, the way an app gets it (docs/12 §2.12):
#   android: release APK for arm64-v8a and x86_64; checks that the core and
#            the module are in it; with EMULATOR=1 (an emulator attached to
#            adb) runs it there against a real agentim
#   ios:     release build for the simulator, then runs it there against a
#            real agentim and waits for "AGENTIM_SMOKE ok" (macOS)
#
#   AGENTIM_BIN=… sh test-app.sh android|ios <dir with the npm tarballs>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
platform=$1 tgz=$(cd "$2" && pwd)
work=$(mktemp -d)
port=$((20900 + $(od -An -N1 -tu1 /dev/urandom)))
cd "$work"
npx --yes @react-native-community/cli@20.2.0 init AgentIMApp --version 0.87.1 \
  --skip-install --skip-git-init --install-pods false --directory app > init.log 2>&1 || { cat init.log; exit 1; }
cd app
npm install --no-audit --no-fund > install.log 2>&1 || { tail -50 install.log; exit 1; }
npm install --no-audit --no-fund "$tgz"/agentimhq-client-web-*.tgz "$tgz"/agentimhq-client-react-native-*.tgz
sed "s/__PORT__/$port/" "$here/test/app/App.tsx" > App.tsx
start_server() {
  "$AGENTIM_BIN" --demo --db "$work/a.db" --addr 127.0.0.1:$port > "$work/server.log" 2>&1 &
  server=$!
  trap 'kill $server 2>/dev/null' EXIT
  for i in $(seq 100); do curl -sf http://127.0.0.1:$port/readyz > /dev/null && break; sleep 0.2; done
}
# The result line from the app's log, waited for up to two minutes.
await_result() { # log file
  for i in $(seq 120); do grep -q "AGENTIM_SMOKE" "$1" && break; sleep 1; done
  grep "AGENTIM_SMOKE" "$1" || { echo "FAIL no result from the app"; return 1; }
  grep -q "AGENTIM_SMOKE ok" "$1"
}
case "$platform" in
android)
  # The test talks plain HTTP to the server on this machine (adb reverse).
  python3 "$here/../allow-cleartext.py" android/app/src/main/AndroidManifest.xml
  (cd android && ./gradlew --no-daemon -q assembleRelease -PreactNativeArchitectures=arm64-v8a,x86_64)
  apk=android/app/build/outputs/apk/release/app-release.apk
  for abi in arm64-v8a x86_64; do
    unzip -l "$apk" | grep -q "lib/$abi/libagentim_client.so" || { echo "FAIL no core for $abi"; exit 1; }
    unzip -l "$apk" | grep -q "lib/$abi/libappmodules.so" || { echo "FAIL no app modules for $abi"; exit 1; }
  done
  grep -rq "NativeAgentIM" android/app/build/generated/autolinking || { echo "FAIL the module is not autolinked"; exit 1; }
  echo "ok   the Android app has the core and the module"
  [ "${EMULATOR:-}" = 1 ] || exit 0
  start_server
  adb wait-for-device
  adb reverse tcp:$port tcp:$port
  adb install -r "$apk" > /dev/null
  adb logcat -c
  adb logcat -s ReactNativeJS:* > "$work/app.log" 2>&1 &
  logger=$!
  adb shell am start -n com.agentimapp/.MainActivity > /dev/null
  await_result "$work/app.log" || { kill $logger; exit 1; }
  kill $logger 2>/dev/null || true
  echo "ok   two people talk through the core in the Android emulator"
  ;;
ios)
  # The package needs iOS 17 (its core is built for it); a new app targets less.
  sed -i '' "s/^platform :ios, .*/platform :ios, '17.0'/" ios/Podfile
  sed -i '' 's/IPHONEOS_DEPLOYMENT_TARGET = [0-9.]*;/IPHONEOS_DEPLOYMENT_TARGET = 17.0;/' ios/AgentIMApp.xcodeproj/project.pbxproj
  (cd ios && bundle install > /dev/null && bundle exec pod install > pod.log 2>&1) || { tail -50 ios/pod.log; exit 1; }
  xcodebuild -quiet -workspace ios/AgentIMApp.xcworkspace -scheme AgentIMApp -configuration Release \
    -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' -derivedDataPath build \
    CODE_SIGNING_ALLOWED=NO build
  app=build/Build/Products/Release-iphonesimulator/AgentIMApp.app
  [ -f "$app/Frameworks/AgentIMCore.framework/AgentIMCore" ] || { echo "FAIL the core is not embedded"; exit 1; }
  echo "ok   the iOS app embeds the core"
  start_server
  udid=$(xcrun simctl list devices available -j | python3 -c "
import json,sys
d=json.load(sys.stdin)['devices']
print(next(x['udid'] for k,v in d.items() if 'iOS' in k for x in v if 'iPhone' in x['name']))")
  xcrun simctl boot "$udid" 2>/dev/null || true
  xcrun simctl bootstatus "$udid" -b > /dev/null
  xcrun simctl install "$udid" "$app"
  log="$work/app.log"
  # Everything the app logs, so a crash or a hang shows why.
  xcrun simctl spawn "$udid" log stream --style compact --level debug --predicate 'process == "AgentIMApp"' > "$log" 2>&1 &
  logger=$!
  sleep 2
  xcrun simctl launch "$udid" org.reactjs.native.example.AgentIMApp > /dev/null
  if ! await_result "$log"; then
    kill $logger 2>/dev/null || true
    tail -n 150 "$log"
    ls "$HOME/Library/Logs/DiagnosticReports" 2>/dev/null | grep -i agentim && tail -n 60 "$HOME"/Library/Logs/DiagnosticReports/AgentIMApp* 2>/dev/null
    exit 1
  fi
  kill $logger 2>/dev/null || true
  echo "ok   two people talk through the core in the iOS simulator"
  ;;
*) echo "usage: $0 android|ios <tarballs>" >&2; exit 2 ;;
esac
