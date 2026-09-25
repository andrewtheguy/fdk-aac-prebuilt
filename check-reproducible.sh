#!/usr/bin/env bash
# Build linux-x86_64 a second time, independently, and require the library to be byte-identical
# to the one ./test-docker.sh linux-x86_64 just built.
#
#   ./check-reproducible.sh        (after ./test-docker.sh linux-x86_64; ci/unix/ci.sh runs both)
#
# Local, on the operator's machine, rather than a CI job comparing two runners through an
# uploaded artifact: this repository is public, so are its workflow artifacts, and an uploaded
# archive is a published one however short its retention. publish-private.sh runs this through
# ci/unix/ci.sh, so every release asserts it.
#
# "Independently" is what makes it worth running. The second build is clean — no build/, no
# dist/, no cmake cache — in a **fresh container** (a new hostname), from a copy of the tree at a
# **different path**, and later in time: a compiler that embedded a path, a hostname or a
# timestamp would pass a rebuild in place and fail this. Same pinned image, because the claim is
# "same source, same flags, same toolchain, same library".
#
# linux-x86_64 only, as before: build.sh's SOURCE_DATE_EPOCH is what makes the archive
# reproducible, cl.exe honours no such variable, and the -v3 and aarch64 archives come from the
# same build.sh and image. The pinned source tarball is copied along rather than downloaded
# again; source.sh checks its SHA-256 either way.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=fdk-aac.env
. ./fdk-aac.env

target=linux-x86_64
tag=fdk-aac-prebuilt-test:amd64
first="dist/$target/lib/libfdk-aac.a"
if [ ! -f "$first" ] || [ ! -f "dist/$target/MANIFEST" ]; then
  echo "no $first — run ./test-docker.sh $target first" >&2
  exit 1
fi

# The engine test-docker.sh chose, by the same rule (see there for the podman details).
engine=("${FDK_AAC_CONTAINER_ENGINE:-}")
if [ -z "${engine[0]}" ]; then
  if command -v podman >/dev/null 2>&1; then engine=(podman)
  elif command -v docker >/dev/null 2>&1; then engine=(docker)
  else echo "neither podman nor docker is on PATH" >&2; exit 1
  fi
fi
if [ "${engine[0]}" = podman ] && [ ! -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/bus" ]; then
  engine+=(--cgroup-manager=cgroupfs)
fi
"${engine[@]}" image inspect "$tag" >/dev/null 2>&1 \
  || { echo "no $tag image — run ./test-docker.sh $target first" >&2; exit 1; }

# The copy: everything but what a build leaves behind, at a path the first build never saw.
scratch="$(mktemp -d)"
copy="$scratch/elsewhere"
mkdir -p "$copy/build"
tar -C "$here" --exclude=./build --exclude=./dist --exclude=./target --exclude=./tmp \
  --exclude=./.git -cf - . | tar -C "$copy" -xf -
cp "build/fdk-aac-${FDK_AAC_VERSION}.tar.gz" "$copy/build/"
# Removed through the container: a rootful docker leaves the second build's files owned by root.
cleanup() {
  "${engine[@]}" run --rm -v "$scratch:/scratch" "$tag" rm -rf /scratch/elsewhere >/dev/null 2>&1 || true
  rm -rf "$scratch"
}
trap cleanup EXIT

echo ">> building $target again: fresh container, clean tree, at /reproduce instead of /work"
"${engine[@]}" run --rm --network host --platform linux/amd64 -v "$copy:/reproduce" -w /reproduce \
  "$tag" ./build.sh "$target" >"$scratch/build.log" 2>&1 || {
  tail -30 "$scratch/build.log" >&2
  echo "the second build of $target failed" >&2
  exit 1
}

sha256_of() { sha256sum "$1" | awk '{print $1}'; }
mine="$(sha256_of "$first")"
theirs="$(sha256_of "$copy/dist/$target/lib/libfdk-aac.a")"
# And each against what its own build recorded, which catches a MANIFEST that disagrees with the
# file beside it.
recorded_mine="$(awk '$1 == "sha256(library)" { print $2 }' "dist/$target/MANIFEST")"
recorded_theirs="$(awk '$1 == "sha256(library)" { print $2 }' "$copy/dist/$target/MANIFEST")"
echo "   first build:  $mine"
echo "   second build: $theirs"
if [ "$recorded_mine" != "$mine" ] || [ "$recorded_theirs" != "$theirs" ]; then
  echo "a MANIFEST disagrees with the library beside it" >&2
  exit 1
fi
[ "$mine" = "$theirs" ] \
  || { echo "the same commit produced two different libraries" >&2; exit 1; }
echo "   reproducible: same source, same flags, same toolchain, same library"
