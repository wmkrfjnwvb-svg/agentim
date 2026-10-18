#!/usr/bin/env bash
# Cluster benchmark (docs/11 C4): the same load against 1 and N nodes on
# one database, one load generator per node, each talking to its node
# (users land on every shard, so most writes are forwarded to the node
# holding the conversation).
#
#   cargo build --release --bins --examples
#   AGENTIM_BENCH_PG=postgres://user:pass@127.0.0.1 \
#   AGENTIM_BENCH_MYSQL=mysql://user:pass@127.0.0.1 bench/cluster.sh
#
# Prints a Markdown report to stdout; raw output goes to bench/out/.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-./target/release/agentim}
LOAD=./target/release/examples/loadtest
OUT=bench/out
NODES=${AGENTIM_BENCH_NODES:-3}
CLIENTS=${AGENTIM_BENCH_CLIENTS:-50}
MESSAGES=${AGENTIM_BENCH_MESSAGES:-200}
mkdir -p "$OUT"
ulimit -n "$(ulimit -Hn)" 2>/dev/null || true

pids=()
stop_nodes() {
  for p in "${pids[@]:-}"; do
    [ -n "$p" ] && kill "$p" 2>/dev/null || true
  done
  for p in "${pids[@]:-}"; do
    [ -n "$p" ] && wait "$p" 2>/dev/null || true
  done
  pids=()
}
trap stop_nodes EXIT

fresh_db() {
  local name="agentim_cbench_$(date +%s)_$RANDOM"
  case "$1" in
    postgres)
      psql "$AGENTIM_BENCH_PG/postgres" -qc "CREATE DATABASE $name" >/dev/null
      echo "$AGENTIM_BENCH_PG/$name" ;;
    mysql)
      local u=${AGENTIM_BENCH_MYSQL#mysql://}
      local cred=${u%@*} host=${u#*@}
      mysql -h"${host%:*}" -u"${cred%%:*}" -p"${cred#*:}" -e "CREATE DATABASE $name" 2>/dev/null
      echo "$AGENTIM_BENCH_MYSQL/$name" ;;
  esac
}

start_node() { # <i> <url>
  local i=$1 cfg="$OUT/cluster-$1.toml"
  printf '[storage]\ncontrol = "main"\n[[storage.databases]]\nname = "main"\nurl = "%s"\nread_pool = 8\n[clients]\nsend_rate = 1000000.0\nsend_burst = 1000000.0\nmax_connections = 100000\n[cluster]\nenabled = true\nnode_id = "node-%s"\nlisten = "127.0.0.1:%s"\nshards = 64\nsecret = "bench-cluster-secret"\n[encryption]\nkey = "dGVzdC1tYXN0ZXIta2V5LWZvci1hZ2VudGltLXRlc3Q="\n' \
    "$2" "$i" $((17946 + i)) > "$cfg"
  "$BIN" --demo --config "$cfg" --addr "127.0.0.1:$((18080 + i))" < /dev/null > "$OUT/cluster-node-$i.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 600); do
    curl -sf -m 1 -o /dev/null "http://127.0.0.1:$((18080 + i))/readyz" && return
    sleep 0.1
  done
  echo "node $i did not become ready" >&2
  cat "$OUT/cluster-node-$i.log" >&2
  exit 1
}

val() { sed -nE "s|.*$2.*|\1|p" "$1" | head -1; }

# run <label> <n> <loadtest args…>: one generator per node, in parallel.
run() {
  local label=$1 n=$2
  shift 2
  local files=() gens=()
  for i in $(seq 1 "$n"); do
    local f="$OUT/cluster-$b-$n-$label-$i.txt"
    files+=("$f")
    "$LOAD" --addr "127.0.0.1:$((18080 + i))" "$@" > "$f" &
    gens+=($!)
  done
  wait "${gens[@]}" || true
  local sum=0 p50=0 p99=0 retried=0
  for f in "${files[@]}"; do
    retried=$((retried + $(val "$f" 'retried: ([0-9]+)')))
    grep -q 'throughput' "$f" || { echo "load generator failed: $f" >&2; cat "$f" >&2; exit 1; }
    sum=$((sum + $(val "$f" 'throughput: ([0-9]+)')))
    p50=$(awk -v a="$p50" -v b="$(val "$f" 'p50 ([0-9.]+)ms')" 'BEGIN{print (b>a)?b:a}')
    p99=$(awk -v a="$p99" -v b="$(val "$f" 'p99 ([0-9.]+)ms')" 'BEGIN{print (b>a)?b:a}')
  done
  echo "$sum msg/s · $p50 · $p99 ms${retried:+ ($retried retried)}"
}

echo "## Cluster benchmark"
echo
echo "Machine: $(nproc) cores, $(lscpu 2>/dev/null | sed -nE 's/^Model name: +//p' | head -1), $(free -g 2>/dev/null | awk '/Mem:/{print $2}') GB RAM. Date: $(date -u +%Y-%m-%d). All nodes, the database and the load generators on the same machine."
echo

backends=""
[ -n "${AGENTIM_BENCH_PG:-}" ] && backends="$backends postgres"
[ -n "${AGENTIM_BENCH_MYSQL:-}" ] && backends="$backends mysql"
for b in $backends; do
  echo "### $b"
  echo
  echo "| Nodes | Write path ($CLIENTS clients per node, own conversations) | Group of 50 per node, all sending |"
  echo "|---|---|---|"
  for n in 1 "$NODES"; do
    url=$(fresh_db "$b")
    for i in $(seq 1 "$n"); do start_node "$i" "$url"; done
    sleep 10 # shards settle on their preferred nodes (joining nodes settle for 5 s)
    w=$(run write "$n" --solo --clients "$CLIENTS" --messages "$MESSAGES")
    g=$(run group "$n" --clients 50 --messages 60)
    echo "| $n | $w | $g |"
    stop_nodes
  done
  echo
done
