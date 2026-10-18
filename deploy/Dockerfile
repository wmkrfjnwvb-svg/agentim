# AgentIM server image (docs/11 C4). Build from the repository root:
#   docker build -f deploy/Dockerfile -t agentim .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY server/ server/
RUN cd server && cargo build --release -p agentim -p agentim-lite \
 && cp target/release/agentim target/release/agentim-lite /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /var/lib/agentim agentim \
 && mkdir -p /var/lib/agentim /etc/agentim && chown agentim /var/lib/agentim
COPY --from=build /usr/local/bin/agentim /usr/local/bin/agentim-lite /usr/local/bin/
USER agentim
WORKDIR /var/lib/agentim
# 8080: clients (WebSocket, HTTP, /metrics, /readyz); 7946: cluster traffic (TLS).
EXPOSE 8080 7946
HEALTHCHECK --interval=10s --timeout=3s CMD curl -fs http://127.0.0.1:8080/readyz || exit 1
ENTRYPOINT ["agentim", "--addr", "0.0.0.0:8080"]
CMD ["--config", "/etc/agentim/agentim.toml"]
