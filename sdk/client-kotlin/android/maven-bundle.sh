#!/bin/sh
# The Maven Central bundle for the Android library (docs/06 D126): the AAR,
# a sources jar, a javadoc jar (the README; Central requires one), the POM,
# and .md5/.sha1 for each, laid out as group/artifact/version for the
# Central Portal's bundle upload. With SIGN=1 each file also gets a .asc
# from the gpg key in the environment (the owner's key, a repository
# secret); without it the bundle is checked but not publishable.
#
#   sh android/maven-bundle.sh <agentim-client.aar> <out dir> [group id]
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
aar=$1 out=$2 group=${3:-com.agentim}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/sdk/client-rust/Cargo.toml" | head -1)
artifact=client
dir="$out/$(echo "$group" | tr . /)/$artifact/$version"
base="$dir/$artifact-$version"
rm -rf "$out" && mkdir -p "$dir"
out=$(cd "$out" && pwd) dir=$(cd "$dir" && pwd)
base="$dir/$artifact-$version"
cp "$aar" "$base.aar"
(cd "$here/src/main/kotlin" && jar cf "$base-sources.jar" .)
docs=$(mktemp -d) && cp "$here/README.md" "$docs/" && (cd "$docs" && jar cf "$base-javadoc.jar" README.md) && rm -rf "$docs"
cat >"$base.pom" <<POM
<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xsi:schemaLocation="http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd">
  <modelVersion>4.0.0</modelVersion>
  <groupId>$group</groupId>
  <artifactId>$artifact</artifactId>
  <version>$version</version>
  <packaging>aar</packaging>
  <name>AgentIM client</name>
  <description>AgentIM client SDK for Android: the Rust core (4 ABIs, 16 KB pages) with a Kotlin API.</description>
  <url>https://github.com/wmkrfjnwvb-svg/agentim</url>
  <licenses>
    <license><name>Apache-2.0</name><url>https://www.apache.org/licenses/LICENSE-2.0.txt</url></license>
  </licenses>
  <developers>
    <developer><name>AgentIM</name><url>https://github.com/wmkrfjnwvb-svg/agentim</url></developer>
  </developers>
  <scm>
    <url>https://github.com/wmkrfjnwvb-svg/agentim</url>
    <connection>scm:git:https://github.com/wmkrfjnwvb-svg/agentim.git</connection>
  </scm>
  <dependencies>
    <dependency>
      <groupId>org.jetbrains.kotlinx</groupId>
      <artifactId>kotlinx-coroutines-core</artifactId>
      <version>1.10.2</version>
      <scope>compile</scope>
    </dependency>
    <dependency>
      <groupId>org.jetbrains.kotlin</groupId>
      <artifactId>kotlin-stdlib</artifactId>
      <version>2.2.20</version>
      <scope>compile</scope>
    </dependency>
  </dependencies>
</project>
POM
for f in "$dir"/*; do
  if [ "${SIGN:-0}" = 1 ]; then gpg --batch --yes --armor --detach-sign "$f"; fi
done
for f in "$dir"/*; do
  case "$f" in *.md5|*.sha1) continue ;; esac
  md5sum "$f" | cut -d' ' -f1 > "$f.md5"
  sha1sum "$f" | cut -d' ' -f1 > "$f.sha1"
done
# Checks: every artifact Central requires is there, the POM is well formed.
for s in .aar -sources.jar -javadoc.jar .pom; do
  [ -s "$base$s" ] && [ -s "$base$s.md5" ] && [ -s "$base$s.sha1" ] || { echo "missing $base$s"; exit 1; }
  if [ "${SIGN:-0}" = 1 ]; then [ -s "$base$s.asc" ] || { echo "unsigned $base$s"; exit 1; }; fi
done
python3 -c "import sys,xml.dom.minidom;xml.dom.minidom.parse(sys.argv[1])" "$base.pom"
(cd "$out" && zip -qr ../maven-bundle.zip .)
echo "bundle $(cd "$out/.." && pwd)/maven-bundle.zip ($group:$artifact:$version, signed: ${SIGN:-0})"
