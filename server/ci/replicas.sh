#!/bin/bash
# Database replicas for tests/replicas.rs (docs/11 §12.5): a PostgreSQL
# primary (port 5440) with a streaming replica (5441), from the local
# binaries, and a MySQL source (3310) with a GTID replica (3311) in docker.
#
#   ci/replicas.sh up pg|mysql
#   ci/replicas.sh pause|resume pg|mysql   the replica stops / goes on applying
#   ci/replicas.sh failover pg|mysql       the primary dies, the replica is promoted
#   ci/replicas.sh rebuild pg|mysql        a new replica of the promoted primary
#   ci/replicas.sh down pg|mysql
set -euo pipefail
D=${REPLICAS_DIR:-/tmp/agentim-replicas}
IMAGE=${MYSQL_IMAGE:-mysql:8.4}
what=$1
db=$2

pgbin() { ls -d /usr/lib/postgresql/*/bin | sort -V | tail -1; }
as_pg() { if [ "$(id -u)" = 0 ]; then runuser -u postgres -- "$@"; else "$@"; fi; }
pg_ctl() { as_pg "$(pgbin)/pg_ctl" "$@"; }

pg_replica() { # $1 = data dir, $2 = port, $3 = port of the primary
  as_pg "$(pgbin)/pg_basebackup" -h 127.0.0.1 -p "$3" -U postgres -D "$1" -R -X stream
  sed -i "s/^port = .*/port = $2/" "$1/postgresql.conf"
  pg_ctl -D "$1" -l "$1.log" -w start >/dev/null
}

my() { # $1 = container, rest = SQL
  local c=$1
  shift
  docker exec "$c" mysql -h127.0.0.1 -uroot -proot -N -e "$*" 2>/dev/null
}

my_start() { # $1 = name, $2 = port, $3 = server id
  docker run -d --name "$1" --network agentim-rep -p "$2:3306" -e MYSQL_ROOT_PASSWORD=root \
    "$IMAGE" --server-id="$3" --gtid-mode=ON --enforce-gtid-consistency=ON --log-bin=bin >/dev/null
  for _ in $(seq 90); do my "$1" "SELECT 1" >/dev/null && return; sleep 1; done
  echo "mysql $1 did not start" >&2
  exit 1
}

my_follow() { # $1 = replica, $2 = source
  my "$1" "SET GLOBAL super_read_only = ON; CHANGE REPLICATION SOURCE TO SOURCE_HOST='$2', SOURCE_USER='root', SOURCE_PASSWORD='root', SOURCE_AUTO_POSITION=1, GET_SOURCE_PUBLIC_KEY=1; START REPLICA;"
}

case "$what/$db" in
up/pg)
  mkdir -p "$D"
  chmod 777 "$D"
  as_pg "$(pgbin)/initdb" -D "$D/pg1" -U postgres --auth=trust >/dev/null
  cat >>"$D/pg1/postgresql.conf" <<EOF
port = 5440
listen_addresses = '127.0.0.1'
unix_socket_directories = '$D'
wal_level = replica
max_wal_senders = 5
wal_keep_size = 256MB
EOF
  echo "host replication all 127.0.0.1/32 trust" >>"$D/pg1/pg_hba.conf"
  pg_ctl -D "$D/pg1" -l "$D/pg1.log" -w start >/dev/null
  pg_replica "$D/pg2" 5441 5440
  ;;
pause/pg) pkill -STOP -f 'walreceiver' ;;
resume/pg) pkill -CONT -f 'walreceiver' ;;
failover/pg)
  pg_ctl -D "$D/pg1" -m immediate -w stop >/dev/null
  pkill -CONT -f 'walreceiver' || true
  pg_ctl -D "$D/pg2" -w promote >/dev/null
  ;;
rebuild/pg)
  rm -rf "$D/pg1"
  pg_replica "$D/pg1" 5440 5441
  ;;
down/pg)
  for d in pg1 pg2; do pg_ctl -D "$D/$d" -m immediate -w stop >/dev/null 2>&1 || true; done
  pkill -CONT -f 'walreceiver' 2>/dev/null || true
  rm -rf "$D/pg1" "$D/pg2"
  ;;
up/mysql)
  docker network create agentim-rep >/dev/null
  my_start agentim-src 3310 1
  my_start agentim-rep 3311 2
  my_follow agentim-rep agentim-src
  ;;
pause/mysql) my agentim-rep "STOP REPLICA SQL_THREAD" ;;
resume/mysql) my agentim-rep "START REPLICA SQL_THREAD" ;;
failover/mysql)
  docker kill agentim-src >/dev/null
  my agentim-rep "START REPLICA SQL_THREAD" || true
  sleep 1
  my agentim-rep "STOP REPLICA; RESET REPLICA ALL; SET GLOBAL super_read_only = OFF; SET GLOBAL read_only = OFF"
  ;;
rebuild/mysql)
  docker rm -f agentim-src >/dev/null
  my_start agentim-src 3310 3
  my_follow agentim-src agentim-rep
  ;;
down/mysql)
  docker rm -f agentim-src agentim-rep >/dev/null 2>&1 || true
  docker network rm agentim-rep >/dev/null 2>&1 || true
  ;;
*)
  echo "usage: $0 up|pause|resume|failover|rebuild|down pg|mysql" >&2
  exit 2
  ;;
esac
