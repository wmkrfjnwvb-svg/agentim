#!/bin/sh
# Builds the core for the browser and copies it next to the JavaScript.
set -e
cd "$(dirname "$0")/.."
(cd ../client-rust && cargo build -p agentim-client-wasm --target wasm32-unknown-unknown --release)
mkdir -p dist
cp ../client-rust/target/wasm32-unknown-unknown/release/agentim_client_wasm.wasm dist/agentim_core.wasm
# The size budget of docs/12 §2.12: 300 KB compressed.
size=$(gzip -9 -c dist/agentim_core.wasm | wc -c)
echo "agentim_core.wasm: $(wc -c < dist/agentim_core.wasm) bytes, $size gzipped"
[ "$size" -le 307200 ] || { echo "over the 300 KB budget"; exit 1; }
