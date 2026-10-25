#!/bin/sh
# Installs the agentim server for this user (docs/12 §1): downloads the
# build for this machine from GitHub Releases, checks its SHA-256 and puts
# it in ~/.agentim/bin. No root needed. Nothing is installed when the
# checksum does not match or cannot be checked.
#
#   curl -fsSL https://github.com/<owner>/agentim/releases/latest/download/install.sh | sh
#
# Settings (environment):
#   AGENTIM_VERSION       a release tag such as v0.2.0 (default: the latest)
#   AGENTIM_INSTALL_DIR   where the program goes (default: ~/.agentim/bin)
#   AGENTIM_DOWNLOAD_URL  where the archives are (default: the GitHub release)
#   AGENTIM_REPO          owner/name on GitHub (set when the release is made)
set -eu

REPO="${AGENTIM_REPO:-__AGENTIM_REPO__}"
VERSION="${AGENTIM_VERSION:-latest}"
INSTALL_DIR="${AGENTIM_INSTALL_DIR:-$HOME/.agentim/bin}"
BASE="${AGENTIM_DOWNLOAD_URL:-}"

say() { printf 'agentim-install: %s\n' "$*"; }
fail() { printf 'agentim-install: error: %s\n' "$*" >&2; exit 1; }

if [ -z "$BASE" ]; then
  case "$REPO" in
    */*) ;;
    *) fail "set AGENTIM_REPO=<owner>/<name> or AGENTIM_DOWNLOAD_URL" ;;
  esac
  if [ "$VERSION" = latest ]; then
    BASE="https://github.com/$REPO/releases/latest/download"
  else
    case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac
    BASE="https://github.com/$REPO/releases/download/$VERSION"
  fi
fi

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  MINGW* | MSYS* | CYGWIN*) fail "on Windows use install.ps1 (PowerShell)" ;;
  *) fail "no build for $(uname -s); build from source (README)" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no build for $(uname -m); build from source (README)" ;;
esac
# A shell under Rosetta reports x86_64 on Apple silicon: take the native one.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] &&
  [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
archive="agentim-$arch-$os.tar.gz"

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  fail "needs curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  fail "needs sha256sum or shasum to check the download; install one and run again"
fi

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t agentim)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $BASE/$archive"
fetch "$BASE/$archive" "$tmp/$archive" || fail "could not download $BASE/$archive"
fetch "$BASE/$archive.sha256" "$tmp/$archive.sha256" || fail "could not download $BASE/$archive.sha256"
want=$(awk '{print $1; exit}' "$tmp/$archive.sha256" | tr 'A-F' 'a-f')
case "$want" in
  *[!0-9a-f]* | "") fail "the checksum file is not a SHA-256" ;;
esac
[ "${#want}" -eq 64 ] || fail "the checksum file is not a SHA-256"
got=$(sha256 "$tmp/$archive")
[ "$got" = "$want" ] || fail "checksum mismatch for $archive (expected $want, got $got); nothing was installed"

mkdir -p "$tmp/x"
tar -xzf "$tmp/$archive" -C "$tmp/x"
[ -f "$tmp/x/agentim" ] || fail "$archive does not contain agentim"
mkdir -p "$INSTALL_DIR"
# Replaced in one step, so a running copy keeps working.
cp "$tmp/x/agentim" "$INSTALL_DIR/.agentim.new"
chmod 755 "$INSTALL_DIR/.agentim.new"
mv -f "$INSTALL_DIR/.agentim.new" "$INSTALL_DIR/agentim"
version=$("$INSTALL_DIR/agentim" --version) || fail "the installed program does not run on this machine"
say "installed $version to $INSTALL_DIR/agentim"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) say "run: agentim" ;;
  *)
    say "$INSTALL_DIR is not on your PATH; add it, for example:"
    say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile"
    say "or run: $INSTALL_DIR/agentim"
    ;;
esac
