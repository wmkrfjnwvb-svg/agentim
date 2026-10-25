#!/bin/sh
# Writes hook/prebuilt.json for the published package (docs/12 §2.12.1):
# the build hook downloads <base_url><file> for the platform being built
# and checks its SHA-256 against this list.
#
#   sh prebuilt.sh <dir of agentim_client-<os>-<arch>.<so|dylib|dll>> <base_url> [out]
set -e
dir=$1 base=$2 out=${3:-$(dirname "$0")/hook/prebuilt.json}
[ -d "$dir" ] && [ -n "$base" ] || { echo "usage: $0 <dir> <base_url>" >&2; exit 2; }
sum() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
{
  printf '{\n  "base_url": "%s",\n  "files": {' "$base"
  sep=""
  for f in "$dir"/agentim_client-*; do
    name=$(basename "$f")
    key=${name#agentim_client-}
    key=${key%.*}
    printf '%s\n    "%s": {"file": "%s", "sha256": "%s"}' "$sep" "$key" "$name" "$(sum "$f")"
    sep=","
  done
  printf '\n  }\n}\n'
} > "$out"
[ -n "$sep" ] || { echo "no agentim_client-* files in $dir" >&2; rm -f "$out"; exit 1; }
echo "wrote $out"
