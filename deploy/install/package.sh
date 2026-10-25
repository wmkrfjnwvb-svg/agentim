#!/bin/sh
# Packs one release archive the way install.sh expects it (docs/12 §1):
# agentim-<target>.tar.gz with the program, LICENSE and README.md at the
# top, and agentim-<target>.tar.gz.sha256 next to it.
#
#   deploy/install/package.sh <target> <path/to/agentim> <out dir>
set -eu
target=$1
bin=$2
out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
name="agentim-$target.tar.gz"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp "$bin" "$stage/agentim"
chmod 755 "$stage/agentim"
cp "$root/LICENSE" "$root/README.md" "$stage/"
mkdir -p "$out"
tar -czf "$out/$name" -C "$stage" agentim LICENSE README.md
cd "$out"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$name" >"$name.sha256"
else
  shasum -a 256 "$name" >"$name.sha256"
fi
cat "$name.sha256"
