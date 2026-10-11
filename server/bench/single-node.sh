#!/usr/bin/env bash
# Single-node benchmark: every scenario against every configured database.
#
#   cargo build --release --bins --examples
#   bench/single-node.sh                      # SQLite only
#   AGENTIM_BENCH_PG=postgres://user:pass@127.0.0.1 \
#   AGENTIM_BENCH_MYSQL=mysql://user:pass@127.0.0.1 bench/single-node.sh
#
# PostgreSQL / MySQL URLs have no database name: a fresh database is created
# for each run (needs the psql / mysql command-line clients). Prints a
# Markdown report to stdout; raw output goes to bench/out/.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-./target/release/agentim}
LOAD=./target/release/examples/loadtest
SCALE=./target/release/examples/scale
OUT=bench/out
PORT=${AGENTIM_BENCH_PORT:-18080}
ADDR=127.0.0.1:$PORT
CONNS=${AGENTIM_BENCH_CONNECTIONS:-10000}
DURATION=${AGENTIM_BENCH_DURATION:-30}
mkdir -p "$OUT"
ulimit -n "$(ulimit -Hn)" 2>/dev/null || true

server_pid=""
stop_server() {
  if [ -n "$server_pid" ]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
    server_pid=""
  fi
}
trap stop_server EXIT

# fresh_db <backend> -> prints a database URL for a new, empty database
fresh_db() {
  local name="agentim_bench_$(date +%s)_$RANDOM"
  case "$1" in
    sqlite)
      rm -f "$OUT/bench.db"*
      echo "sqlite://$PWD/$OUT/bench.db?mode=rwc" ;;
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

ready_secs=""
start_server() { # <url> <log>; sets server_pid and ready_secs
  if curl -s -m 1 -o /dev/null "http://$ADDR/healthz"; then
    echo "port $PORT is already in use; stop the other server first" >&2; exit 1
  fi
  printf '[storage]\ncontrol = "main"\n[[storage.databases]]\nname = "main"\nurl = "%s"\nread_pool = 8\n' "$1" > "$OUT/bench.toml"
  local t0=$(date +%s%N)
  "$BIN" --demo --config "$OUT/bench.toml" --addr "$ADDR" < /dev/null > "$2" 2>&1 &
  server_pid=$!
  for _ in $(seq 600); do
    kill -0 "$server_pid" 2>/dev/null || { echo "server exited:" >&2; cat "$2" >&2; exit 1; }
    if curl -sf -m 1 -o /dev/null "http://$ADDR/readyz"; then
      ready_secs=$(( ($(date +%s%N) - t0) / 1000000 ))
      return
    fi
    sleep 0.05
  done
  echo "server did not become ready" >&2; exit 1
}

val() { # <file> <regex with one group>
  sed -nE "s|.*$2.*|\1|p" "$1" | head -1
}

echo "## Single-node benchmark"
echo
echo "Machine: $(nproc) cores, $(lscpu 2>/dev/null | sed -nE 's/^Model name: +//p' | head -1), $(free -g 2>/dev/null | awk '/Mem:/{print $2}') GB RAM. Date: $(date -u +%Y-%m-%d). Load generator on the same machine."
echo

backends=${AGENTIM_BENCH_ONLY:-sqlite}
if [ -z "${AGENTIM_BENCH_ONLY:-}" ]; then
  [ -n "${AGENTIM_BENCH_PG:-}" ] && backends="$backends postgres"
  [ -n "${AGENTIM_BENCH_MYSQL:-}" ] && backends="$backends mysql"
fi

for b in $backends; do
  echo "### $b"
  echo
  echo "| Scenario | Throughput | ACK p50 | ACK p99 | Notes |"
  echo "|---|---|---|---|---|"
  url=$(fresh_db "$b")
  start_server "$url" "$OUT/$b-server.log"

  # Write path: every client in its own conversation.
  for spec in "1 500" "10 300" "50 200" "200 100"; do
    set -- $spec
    f="$OUT/$b-write-$1.txt"
    "$LOAD" --addr "$ADDR" --solo --clients "$1" --messages "$2" > "$f"
    echo "| Write path, $1 client(s) | $(val "$f" 'throughput: ([0-9]+)') msg/s | $(val "$f" 'p50 ([0-9.]+)ms') ms | $(val "$f" 'p99 ([0-9.]+)ms') ms | every client in its own conversation; retries deduplicated |"
  done

  # Fan-out: one group, concurrent senders and streaming agents.
  for spec in "50 100 10" "200 30 10"; do
    set -- $spec
    f="$OUT/$b-fanout-$1.txt"
    "$LOAD" --addr "$ADDR" --clients "$1" --messages "$2" --streams "$3" --chunks 60 > "$f"
    pushes=$(val "$f" '; ([0-9]+) pushed events')
    echo "| Group of $1, all sending, $3 agents streaming | $(val "$f" 'throughput: ([0-9]+)') msg/s | $(val "$f" 'p50 ([0-9.]+)ms') ms | $(val "$f" 'p99 ([0-9.]+)ms') ms | $pushes pushes delivered in order |"
  done
  stop_server

  # Scale scenarios, each on a fresh server.
  run_scale() { # <name> <label> <args...>
    local name=$1 label=$2; shift 2
    start_server "$url" "$OUT/$b-$name-server.log"
    local f="$OUT/$b-$name.txt"
    "$SCALE" --addr "$ADDR" --duration "$DURATION" --server-pid "$server_pid" "$@" > "$f" || true
    local status=$(grep -q '^OK' "$f" && echo ok || echo FAIL)
    local rate=$(val "$f" '\(([0-9]+) msg/s\)')
    local e2e="p50 $(val "$f" 'delivery: +p50 ([0-9]+)ms') ms, p99 $(val "$f" 'delivery: +p50 [0-9]+ms +p99 ([0-9]+)ms') ms"
    local mem=$(val "$f" '\(([0-9.]+) KB per connection\)')
    local extra=$(val "$f" 'storm: .* in ([0-9.]+s)')
    echo "| $label | $rate msg/s | $(val "$f" 'ack latency: +p50 ([0-9]+)ms') ms | $(val "$f" 'ack latency: +p50 [0-9]+ms +p99 ([0-9]+)ms') ms | delivery $e2e; ${mem:+$mem KB server memory per connection; }${extra:+all reconnected in $extra; }$status |"
    stop_server
  }
  run_scale connections "$CONNS connections, groups of 100, 1% sending; reconnect storm" --scenario connections --users "$CONNS" --rate 100 --storm --sources 4
  run_scale biggroup "1000-member group, 50 msg/s" --scenario biggroup --users 1000 --rate 50
  run_scale slow "200-member group, 20 stop reading, 2 KB messages" --scenario slow --users 200 --rate 300 --payload 2000

  # Recovery: kill -9 with everything above in the database, time to ready.
  start_server "$url" "$OUT/$b-recover-server.log"
  kill -9 "$server_pid"; wait "$server_pid" 2>/dev/null || true; server_pid=""
  start_server "$url" "$OUT/$b-recover2-server.log"
  echo "| Restart after kill -9 | | | | ready in ${ready_secs} ms (process start to /readyz) with all the data above in the database |"
  stop_server
  echo
done
