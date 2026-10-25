#!/bin/sh
# Builds a new React Native 0.87.1 app with the package installed from its
# npm tarball, the way an app gets it (docs/12 §2.12):
#   android: release APK for arm64-v8a and x86_64; checks that the core and
#            the module are in it
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
case "$platform" in
android)
  (cd android && ./gradlew --no-daemon -q assembleRelease -PreactNativeArchitectures=arm64-v8a,x86_64)
  apk=android/app/build/outputs/apk/release/app-release.apk
  for abi in arm64-v8a x86_64; do
    unzip -l "$apk" | grep -q "lib/$abi/libagentim_client.so" || { echo "FAIL no core for $abi"; exit 1; }
    unzip -l "$apk" | grep -q "lib/$abi/libappmodules.so" || { echo "FAIL no app modules for $abi"; exit 1; }
  done
  grep -rq "NativeAgentIM" android/app/build/generated/autolinking || { echo "FAIL the module is not autolinked"; exit 1; }
  echo "ok   the Android app has the core and the module"
  ;;
ios)
  (cd ios && bundle install > /dev/null && bundle exec pod install > pod.log 2>&1) || { tail -50 ios/pod.log; exit 1; }
  xcodebuild -quiet -workspace ios/AgentIMApp.xcworkspace -scheme AgentIMApp -configuration Release \
    -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' -derivedDataPath build \
    CODE_SIGNING_ALLOWED=NO build
  app=build/Build/Products/Release-iphonesimulator/AgentIMApp.app
  [ -f "$app/Frameworks/AgentIMCore.framework/AgentIMCore" ] || { echo "FAIL the core is not embedded"; exit 1; }
  echo "ok   the iOS app embeds the core"
  "$AGENTIM_BIN" --demo --db "$work/a.db" --addr 127.0.0.1:$port > "$work/server.log" 2>&1 &
  server=$!
  trap 'kill $server 2>/dev/null' EXIT
  for i in $(seq 100); do curl -sf http://127.0.0.1:$port/readyz > /dev/null && break; sleep 0.2; done
  udid=$(xcrun simctl list devices available -j | python3 -c "
import json,sys
d=json.load(sys.stdin)['devices']
print(next(x['udid'] for k,v in d.items() if 'iOS' in k for x in v if 'iPhone' in x['name']))")
  xcrun simctl boot "$udid" 2>/dev/null || true
  xcrun simctl bootstatus "$udid" -b > /dev/null
  xcrun simctl install "$udid" "$app"
  log="$work/app.log"
  xcrun simctl spawn "$udid" log stream --style compact --predicate 'eventMessage CONTAINS "AGENTIM_SMOKE"' > "$log" 2>&1 &
  logger=$!
  sleep 2
  xcrun simctl launch "$udid" org.reactjs.native.example.AgentIMApp > /dev/null
  for i in $(seq 120); do grep -q "AGENTIM_SMOKE" "$log" && break; sleep 1; done
  kill $logger 2>/dev/null || true
  grep "AGENTIM_SMOKE" "$log" || { echo "FAIL no result from the app"; exit 1; }
  grep -q "AGENTIM_SMOKE ok" "$log" || exit 1
  echo "ok   two people talk through the core in the iOS simulator"
  ;;
*) echo "usage: $0 android|ios <tarballs>" >&2; exit 2 ;;
esac
