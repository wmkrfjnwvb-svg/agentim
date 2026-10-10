#!/usr/bin/env bash
# kill -9 the server while clients are sending; restart; verify that every
# message that was ACKed before the crash is still in the log.
set -euo pipefail
BIN=${BIN:-target/release/agentim}
LT=${LT:-target/release/examples/loadtest}
WORK=$(mktemp -d)
ADDR=127.0.0.1:${PORT:-18090}
trap 'kill -9 $(jobs -p) 2>/dev/null || true' EXIT

wait_up() { for _ in $(seq 50); do curl -sf "http://$ADDR/healthz" >/dev/null && return; sleep 0.1; done; echo "server did not start"; exit 1; }

"$BIN" --demo --db "$WORK/db" --addr "$ADDR" >"$WORK/server1.log" 2>&1 &
PID=$!
wait_up
"$LT" --addr "$ADDR" --clients 20 --ack-log "$WORK/acks" &
LTPID=$!
sleep "${CRASH_AFTER:-2}"
kill -9 "$PID"
wait "$LTPID" || true
echo "acked before crash: $(($(wc -l <"$WORK/acks") - 1))"

"$BIN" --demo --db "$WORK/db" --addr "$ADDR" >"$WORK/server2.log" 2>&1 &
wait_up
"$LT" --addr "$ADDR" --verify-ack-log "$WORK/acks"
