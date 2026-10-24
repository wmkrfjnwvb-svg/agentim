# @agentim/client-react-native

The AgentIM client for React Native (docs/12 §2.12). The Rust core runs natively and is reached
through JSI (`cpp/AgentIMJsi.cpp` installs `global.__agentim`). The API is the same as
`@agentim/client-web`, so the hooks of `@agentim/react` work unchanged.

```ts
import { AgentIMClient } from '@agentim/client-react-native';

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

## Native setup (not built here)

The app links `libagentim_client` (Android: 4 ABIs with 16 KB pages; iOS: the XCFramework) and
compiles `cpp/` into its TurboModule. Its `install` hook calls
`agentim::install(runtime, jsCallInvoker)`. This environment has no Android or iOS toolchain, so
that packaging and a device run are not done yet.

## Tests

- `npm test`: the TypeScript side against a fake binding (config, tokens, reply order,
  errors, timeouts, listeners, dispose).
- The binding is compiled with `-Wall -Wextra -Werror` against the JSI headers of
  `react-native` 0.87.1 (CI).
