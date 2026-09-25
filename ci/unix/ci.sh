#!/usr/bin/env bash
# Build and gate the archives this machine can make — the half of `./publish-private.sh`
# that runs on the machine doing the building, put there by ci/unix/remote.sh, which leaves
# `dist/<target>/` behind in the workspace for the publisher to fetch.
#
# The gate is build.yml's: build.sh's own verification, the workspace's tests against the
# archive, and the e2e binary run and checked for what it links. On Linux x86_64 it also rebuilds
# linux-x86_64 independently and requires the same library (check-reproducible.sh) — a check
# that lives here rather than on GitHub, which would have to upload the archive to compare it.
#
#   macOS arm64    macos-arm64, natively.
#   Linux x86_64   linux-x86_64 and linux-x86_64-v3, in a container (test-docker.sh).
#   Linux aarch64  linux-aarch64, in a container.
#
# Linux always builds in a container, so every Linux archive comes out of the same pinned
# toolchain whichever machine ran it — and so that the machine needs nothing installed but
# docker or podman, which is what lets a server that is nobody's build box make one.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    target_dir="${CARGO_TARGET_DIR:-target}"
    ./build.sh macos-arm64
    ./sync-prebuilt.sh
    cargo test --offline --workspace
    cargo build --offline --release --workspace
    "$target_dir/release/fdk-aac-e2e"
    ./check-static.sh "$target_dir/release/fdk-aac-e2e"
    ;;
  Linux-x86_64)
    ./test-docker.sh linux-x86_64
    ./test-docker.sh linux-x86_64-v3
    ./check-reproducible.sh
    ;;
  Linux-aarch64 | Linux-arm64)
    ./test-docker.sh linux-aarch64
    ;;
  *)
    echo "no target of build.sh's builds on $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac
