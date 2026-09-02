#!/usr/bin/env bash
# Build the Linux archives and run the Rust test suite against them, in Docker.
#
# Usage:
#   ./test-docker.sh                 # every Linux target this machine can run
#   ./test-docker.sh linux-aarch64   # just one
#
# Why Docker and not CI: the Linux artifacts are the ones a developer on a Mac cannot
# otherwise touch, and they are also where the interesting measurement happens — build.sh
# decides whether the archive needs a C++ runtime by looking at its undefined symbols, and
# that answer drives both the link flags every consumer gets and whether the musl targets
# are supported at all. Ten minutes locally beats finding out on a tag.
#
# On Apple silicon, `linux/aarch64` runs natively and is fast; `linux/amd64` is emulated and
# is not. Read the note under "emulation" below before believing a failure there.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

image=fdk-aac-prebuilt-test

# target : docker platform.
targets=(
  "linux-aarch64:linux/arm64"
  "linux-x86_64:linux/amd64"
)

want="${1:-}"
if [ -n "$want" ]; then
  # shellcheck disable=SC2076
  [[ " ${targets[*]} " =~ " $want:" ]] || {
    echo "not a Linux target: $want" >&2
    printf '  %s\n' "${targets[@]%%:*}" >&2
    exit 1
  }
fi

# cmake for build.sh, and that is the only reason it is in here — nothing in the *Rust* half
# needs it, which is the property this whole repository exists to give consumers.
#
# Once per platform, and tagged per platform: an image built for arm64 cannot be run with
# `--platform linux/amd64`, and docker's response to being asked is to try to *pull* the tag,
# which fails with a confusing authentication error.
build_image() {
  local platform="$1" tag="$image:${1##*/}"
  docker image inspect "$tag" >/dev/null 2>&1 && return 0
  echo ">> building the test image for $platform"
  docker build -q --platform "$platform" -t "$tag" -f - . <<'DOCKERFILE' >/dev/null
FROM rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake binutils valgrind \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /work
DOCKERFILE
}

status=0
for entry in "${targets[@]}"; do
  target="${entry%%:*}"
  platform="${entry##*:}"
  [ -z "$want" ] || [ "$want" = "$target" ] || continue

  echo
  echo "=============== $target ($platform)"
  build_image "$platform" || {
    status=1
    echo "--- $target SKIPPED: no image for $platform" >&2
    echo "    On Apple silicon, linux/amd64 needs the qemu binfmt handler that Docker" >&2
    echo "    Desktop installs; the GitHub Actions run covers this target either way." >&2
    continue
  }
  # A per-platform CARGO_TARGET_DIR, because the host's target/ holds the host's artifacts
  # and cargo would rebuild the world on every switch — or worse, try to reuse them.
  # `build/` and `dist/` are already per-target, so those are safe to share, and sharing
  # them means the fdk-aac tarball is downloaded once for all of this.
  if docker run --rm --platform "$platform" \
      -v "$here:/work" \
      -e CARGO_TARGET_DIR="/work/target/docker-${platform##*/}" \
      "$image:${platform##*/}" bash -euo pipefail -c "
        ./build.sh $target
        ./sync-prebuilt.sh
        # --offline proves the point of the exercise: after sync-prebuilt.sh there is
        # nothing left to fetch, so a consumer's build needs no network either.
        cargo test --offline --workspace
        # The same end-to-end leg the pipeline runs: a release binary, executed, then
        # checked for a dynamic fdk-aac dependency it must not have — and for a C++ runtime
        # dependency the MANIFEST says it should not need.
        cargo build --offline --release --workspace
        ./target/docker-${platform##*/}/release/fdk-aac-e2e | tail -4
        ./check-static.sh ./target/docker-${platform##*/}/release/fdk-aac-e2e

        # Leak checking, exactly rather than by watching RSS. valgrind interposes on malloc,
        # so it sees the allocations fdk-aac makes in C through libSYS — which is the whole
        # difficulty: a Rust GlobalAlloc counter observes only Rust-side allocation and would
        # report a clean run no matter how badly the handles leaked.
        #
        # definite,indirect and not 'possible': Rust's runtime holds interior pointers that
        # valgrind reasonably calls possibly-lost, and a missing aacEncClose is always
        # definitely-lost, so the narrower set is both quieter and sufficient.
        echo '>> valgrind'
        bin=./target/docker-${platform##*/}/release/fdk-aac-e2e
        FDK_AAC_E2E_MEMORY=off valgrind --leak-check=full \\
          --errors-for-leak-kinds=definite,indirect --error-exitcode=1 \\
          \"\$bin\" >/dev/null 2>valgrind.log \\
          || { echo '   valgrind found a leak:'; grep -E 'lost|ERROR SUMMARY' valgrind.log; exit 1; }

        # The control. A clean valgrind run means nothing unless valgrind can see these
        # allocations at all, so point it at a process that leaks on purpose and require it
        # to fail. Passing here would mean the check above proves nothing.
        if FDK_AAC_E2E_MEMORY=off valgrind --leak-check=full \\
             --errors-for-leak-kinds=definite,indirect --error-exitcode=1 \\
             \"\$bin\" --leak-only >/dev/null 2>&1; then
          echo '   valgrind did not notice 60 deliberately leaked codecs — the check is blind' >&2
          exit 1
        fi
        echo '   no leaks, and the control is detected'
        rm -f valgrind.log

        echo
        cat dist/$target/MANIFEST
      "; then
    echo "--- $target OK"
  else
    code=$?
    status=1
    echo "--- $target FAILED (exit $code)" >&2
    # 132 is SIGILL, and with linux-x86_64 built to the x86-64 baseline it is never expected,
    # emulated or not — every emulator implements SSE2. When this archive was built to
    # x86-64-v3, Rosetta's missing AVX2 made a correct artifact die here; now a 132 means an
    # instruction above the baseline got into the archive despite build.sh's check, or the
    # e2e binary itself was built with a floor. Either is a real finding, not the emulator's.
    if [ "$code" = 132 ] && [ "$platform" = linux/amd64 ]; then
      echo "    SIGILL. linux-x86_64 is built to the x86-64 baseline, so this is not an" >&2
      echo "    emulator limit: look for an instruction above SSE2 in the archive" >&2
      echo "    (objdump -d dist/linux-x86_64/lib/libfdk-aac.a | grep -E '\\sv[a-z]') or a" >&2
      echo "    RUSTFLAGS target-cpu on the e2e binary." >&2
    fi
  fi
done

exit "$status"
