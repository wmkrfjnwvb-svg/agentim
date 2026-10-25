#!/bin/sh
# Tests install.sh for real (docs/12 §1): serves a release folder over
# HTTP, installs from it into an empty home, starts the installed program
# and sends a first message; a corrupted archive installs nothing.
#
#   deploy/install/test.sh <release dir with agentim-<target>.tar.gz[.sha256]>
set -eu
dist=$(cd "$1" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$dist" >"$work/http.log" 2>&1 &
http=$!
trap 'kill $http 2>/dev/null || true; rm -rf "$work"' EXIT
for _ in $(seq 1 50); do
  curl -fs "http://127.0.0.1:$port/" >/dev/null 2>&1 && break
  sleep 0.1
done

echo "== install"
HOME="$work/home" AGENTIM_DOWNLOAD_URL="http://127.0.0.1:$port" sh "$here/install.sh" | tee "$work/out.txt"
bin="$work/home/.agentim/bin/agentim"
test -x "$bin"
grep -q "is not on your PATH" "$work/out.txt"
"$bin" --version
python3 "$root/deploy/smoke/first_message.py" "$bin"

echo "== install again (an update replaces the program)"
HOME="$work/home" AGENTIM_DOWNLOAD_URL="http://127.0.0.1:$port" sh "$here/install.sh" >/dev/null
test -x "$bin"

echo "== a corrupted archive installs nothing"
bad="$work/bad"
mkdir -p "$bad"
cp "$dist"/* "$bad/"
for f in "$bad"/*.tar.gz; do printf 'x' >>"$f"; done
kill $http
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$bad" >"$work/http.log" 2>&1 &
http=$!
for _ in $(seq 1 50); do
  curl -fs "http://127.0.0.1:$port/" >/dev/null 2>&1 && break
  sleep 0.1
done
if HOME="$work/fresh" AGENTIM_DOWNLOAD_URL="http://127.0.0.1:$port" sh "$here/install.sh" >"$work/bad.txt" 2>&1; then
  cat "$work/bad.txt"
  echo "a corrupted archive was installed" >&2
  exit 1
fi
grep -q "checksum mismatch" "$work/bad.txt"
test ! -e "$work/fresh/.agentim/bin/agentim"

echo "== no download source"
if HOME="$work/fresh" sh "$here/install.sh" >"$work/none.txt" 2>&1; then
  echo "installed without a repository" >&2
  exit 1
fi
grep -q "AGENTIM_REPO" "$work/none.txt"
echo "install.sh: all checks passed"
