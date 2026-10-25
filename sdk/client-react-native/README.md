# @agentimhq/client-react-native

The AgentIM client for React Native (docs/12 §2.12). The Rust core runs natively and is reached
through JSI (`cpp/AgentIMJsi.cpp` installs `global.__agentim`). The API is the same as
`@agentimhq/client-web`, so the hooks of `@agentimhq/react` work unchanged.

```ts
import { AgentIMClient } from '@agentimhq/client-react-native';

const client = AgentIMClient.create({
  url: 'https://chat.example.com',
  deviceId: savedDeviceId,
  storeDir: `${DocumentDirectoryPath}/agentim`,   // none: in memory
  tokenProvider: () => backend.token(),           // throw TokenRefused to stop
});
AppState.addEventListener('change', (s) => client.setForeground(s === 'active'));
NetInfo.addEventListener((s) => client.setNetwork(!!s.isConnected));
client.connect();
```

Events come from the SDK's thread. The binding copies each one and hands it to the JS thread
through the `CallInvoker`, in order. `dispose()` stops the client; after it returns, no event
arrives. Dispose clients before a JS reload.

## Native setup

Nothing to do beyond `npm install` (React Native 0.76+, the new architecture; iOS 17+):

- The package is a pure C++ TurboModule (`AgentIM`, `cpp/NativeAgentIM.cpp`) that installs the
  JSI binding. Autolinking builds it into the app: Android from `android/CMakeLists.txt`
  (`react-native.config.cjs`), iOS from `AgentIMReactNative.podspec` (`pod install`).
- The native core ships in the package: `android/jniLibs/<abi>/libagentim_client.so`
  (arm64-v8a, armeabi-v7a, x86_64, x86; 16 KB pages) and `ios/AgentIMCore.xcframework`
  (dynamic, with its privacy manifest). `prepare-native.sh` puts them there before `npm pack`.
- Apps get `src/react-native.ts` (the `react-native` field), which calls the module's
  `install()` once; Node and type checking get `dist/index.js`.

## Tests

- `npm test`: the TypeScript side against a fake binding (config, tokens, reply order,
  errors, timeouts, listeners, dispose).
- The binding is compiled with `-Wall -Wextra -Werror` against the JSI headers of
  `react-native` 0.87.1 (CI).
- `test-app.sh android|ios <tarballs>`: a new React Native 0.87.1 app with the package installed
  from its tarball. Android: a release APK for arm64-v8a and x86_64 with the core and the module
  in it. iOS: a release build for the simulator that embeds the core, run there against a real
  `agentim`: two people connect and one gets the other's message (CI, `sdk-packages.yml`).
