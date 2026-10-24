#!/bin/sh
# Assembles the Unity package: the same sources as the NuGet package, plus
# the native libraries built per platform (docs/12 §2.12).
set -e
cd "$(dirname "$0")"
cp ../AgentIM.Client/Client.cs ../AgentIM.Client/Native.cs Runtime/
mkdir -p Runtime/Plugins
for f in "$@"; do cp "$f" Runtime/Plugins/; done
