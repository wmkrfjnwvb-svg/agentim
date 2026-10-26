#!/bin/sh
# Builds the demo (docs/12 §3.4) into server/demo/dist, which build.rs
# embeds into agentim: first the SDK packages it is made of (the browser
# client with its wasm core, and the React components), then the app.
# WASM=<file> uses a core built elsewhere instead of building it here
# (the Docker image builds it in its Rust stage).
set -e
cd "$(dirname "$0")"
(
  cd ../../sdk/client-web
  npm ci
  if [ -n "$WASM" ]; then
    npx tsc -p tsconfig.json
    mkdir -p dist && cp "$WASM" dist/agentim_core.wasm
  else
    npm run build
  fi
)
(cd ../../sdk/react && npm ci && npm run build)
npm ci
npm run build
