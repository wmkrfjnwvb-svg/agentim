#!/bin/sh
# The version being released (docs/12 §1). The program, the Helm chart and
# CHANGELOG.md must agree; with a tag (v1.2.3) it must match too.
#
#   deploy/release/version.sh [tag]      prints the version
#   deploy/release/version.sh --notes X  prints CHANGELOG.md's section for X
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
if [ "${1:-}" = --notes ]; then
  awk -v v="$2" '
    $0 ~ "^## \\[" v "\\]" { on = 1; next }
    on && /^## \[/ { exit }
    on { print }
  ' "$root/CHANGELOG.md"
  exit 0
fi
cargo=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/server/crates/agentim/Cargo.toml" | head -1)
chart=$(sed -n 's/^version: *//p' "$root/deploy/helm/agentim/Chart.yaml")
app=$(sed -n 's/^appVersion: *"\{0,1\}\([^"]*\)"\{0,1\}/\1/p' "$root/deploy/helm/agentim/Chart.yaml")
fail() { echo "version: $*" >&2; exit 1; }
[ -n "$cargo" ] || fail "no version in server/crates/agentim/Cargo.toml"
[ "$chart" = "$cargo" ] || fail "Chart.yaml version $chart, the program $cargo"
[ "$app" = "$cargo" ] || fail "Chart.yaml appVersion $app, the program $cargo"
if [ -n "${1:-}" ]; then
  [ "$1" = "v$cargo" ] || fail "tag $1, the program is $cargo"
  grep -q "^## \[$cargo\]" "$root/CHANGELOG.md" || fail "CHANGELOG.md has no section [$cargo]"
fi
echo "$cargo"
