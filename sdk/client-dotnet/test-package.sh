#!/bin/sh
# Installs the AgentIM.Client package from a local folder into the
# conformance app and runs the scenario against a real server: the native
# library .NET loads is the one the package ships for this platform
# (docs/12 §2.12.1).
#
#   AGENTIM_BIN=… sh test-package.sh <folder with AgentIM.Client.<version>.nupkg>
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=$(cd "$1" && pwd)
pkg=$(ls "$src"/AgentIM.Client.*.nupkg | head -1)
version=$(basename "$pkg" .nupkg | sed 's/^AgentIM\.Client\.//')
# A fresh package cache: nothing left over from a project build.
export NUGET_PACKAGES="${TMPDIR:-/tmp}/agentim-nuget-$$"
dotnet run --project "$here/Conformance" -p:AgentIMPackage="$version" \
  -p:RestoreAdditionalProjectSources="$src"
