# AgentIM server image (docs/11 C4, docs/12 §1). Build from the repository root:
#   docker build -f deploy/Dockerfile -t agentim .
# Releases use the `release` stage with the static binaries the release
# workflow built (no compiler in the image build, the same binary as the
# downloads):
#   docker buildx build -f deploy/Dockerfile --target release \
#     --platform linux/amd64,linux/arm64 --build-context bin=<dir with amd64/ and arm64/> .
FROM node:22-bookworm-slim AS console
WORKDIR /src/server/console
COPY server/console/package.json server/console/package-lock.json ./
RUN npm ci
COPY server/console/ ./
RUN npm run build

# The demo (docs/12 §3.4) runs the client core in the browser: build it
# for wasm here, then the demo on the browser SDK and the components.
FROM rust:1-bookworm AS wasm
WORKDIR /src
COPY sdk/client-rust/ sdk/client-rust/
RUN rustup target add wasm32-unknown-unknown \
 && cd sdk/client-rust && cargo build --locked -p agentim-client-wasm --target wasm32-unknown-unknown --release

FROM node:22-bookworm-slim AS demo
WORKDIR /src
COPY sdk/client-web/ sdk/client-web/
COPY sdk/react/ sdk/react/
COPY server/demo/ server/demo/
COPY --from=wasm /src/sdk/client-rust/target/wasm32-unknown-unknown/release/agentim_client_wasm.wasm /tmp/core.wasm
RUN WASM=/tmp/core.wasm sh server/demo/build.sh

FROM rust:1-bookworm AS build
WORKDIR /src
COPY server/ server/
# The console and the demo are embedded in the binary (docs/12 §4, §3.4).
COPY --from=console /src/server/console/dist server/console/dist
COPY --from=demo /src/server/demo/dist server/demo/dist
RUN cd server && cargo build --release -p agentim \
 && cp target/release/agentim /usr/local/bin/

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 agentim \
 && useradd --system --uid 10001 --gid 10001 --home /var/lib/agentim agentim \
 && mkdir -p /var/lib/agentim /etc/agentim && chown agentim /var/lib/agentim
WORKDIR /var/lib/agentim
# 8080: clients (WebSocket, HTTP, /metrics, /readyz); 7946: cluster traffic (TLS).
EXPOSE 8080 7946
HEALTHCHECK --interval=10s --timeout=3s CMD curl -fs http://127.0.0.1:8080/readyz || exit 1
ENTRYPOINT ["agentim", "--addr", "0.0.0.0:8080"]
CMD ["--config", "/etc/agentim/agentim.toml"]

FROM runtime AS release
ARG TARGETARCH
COPY --from=bin --chmod=755 ${TARGETARCH}/agentim /usr/local/bin/agentim
USER agentim

FROM runtime
COPY --from=build /usr/local/bin/agentim /usr/local/bin/
USER agentim
