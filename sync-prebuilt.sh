#!/usr/bin/env bash
# Connect the shell half of this repo to the Rust half.
#
# Usage:
#   ./sync-prebuilt.sh              copy dist/* into the crate's prebuilt/ cache
#   ./sync-prebuilt.sh --headers    refresh the committed headers and both generated files
#   ./sync-prebuilt.sh --check      verify the committed headers, bindings and version consts
#   ./sync-prebuilt.sh --fetch      download the latest release's archives into prebuilt/
#
# Neither `prebuilt/` nor `dist/` is committed — see .gitignore. Three things *are*:
#
#   include/fdk-aac/   the six public headers, byte-identical to the pinned tarball's.
#                      Text, small, and reviewable — the opposite of a committed `.a`.
#   src/bindings.rs    generated *from* those headers by gen-bindings.sh.
#   src/version.rs     generated from the pinned .cpp sources by gen-version.sh.
#
# Which is a chain: fdk-aac.env pins a checksum, the checksum gates the tarball, the tarball
# is where the headers come from, and the headers are where the bindings come from. It holds
# only if every link is checked, so `--check` checks all of them and CI runs it.
#
# There is nothing here to pin a release with. build.rs fetches from the repository's latest
# release, so publishing one is the whole of releasing — no follow-up commit restating what
# GitHub already serves.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=fdk-aac.env
. ./fdk-aac.env
# shellcheck source=source.sh
. ./source.sh

crate=crates/fdk-aac-prebuilt-sys
prebuilt="$crate/prebuilt"

# Every target build.sh knows how to make, both x86_64 flavours included.
targets=(macos-arm64 linux-x86_64 linux-x86_64-v3 linux-aarch64
         windows-x86_64-msvc windows-x86_64-msvc-v3)

# The six headers cmake installs, at the paths they live at in the source tree. Named here
# rather than globbed, because "every .h under libSYS" is a different and much larger set —
# fdk-aac's modules carry dozens of internal headers, and shipping those would present as
# public interface things upstream is free to change.
headers=(
  libAACenc/include/aacenc_lib.h
  libAACdec/include/aacdecoder_lib.h
  libSYS/include/FDK_audio.h
  libSYS/include/genericStds.h
  libSYS/include/machine_type.h
  libSYS/include/syslib_channelMapDescr.h
)

case "${1:-}" in
  --headers | --check)
    # Both modes need the pinned tree, and getting it goes through the same checksum gate
    # build.sh uses — headers that were never verified would make the generated bindings
    # unverified too, and those are what every FFI call in the crate above is shaped by.
    ensure_source
    src="build/fdk-aac-${FDK_AAC_VERSION}"

    if [ "$1" = "--headers" ]; then
      rm -rf "$crate/include/fdk-aac"
      mkdir -p "$crate/include/fdk-aac"
      for header in "${headers[@]}"; do
        cp "$src/$header" "$crate/include/fdk-aac/"
      done
      # Fraunhofer's licence, from the same verified tarball, in both places it belongs:
      # beside the headers it covers, and at the repository root where anyone looks first.
      cp "$src/NOTICE" "$crate/include/NOTICE"
      cp "$src/NOTICE" LICENSE
      echo ">> $crate/include/fdk-aac is now fdk-aac $FDK_AAC_VERSION's headers"
      (cd "$crate" && ./gen-bindings.sh && ./gen-version.sh)
      exit 0
    fi

    echo ">> comparing the committed headers against fdk-aac $FDK_AAC_VERSION"
    # Staged into a directory of nothing but the six headers, then compared as directories:
    # that way one `diff -r` covers changed, missing *and* extra files. A header that should
    # not be there matters as much as one that was edited, because bindings.rs is generated
    # from whatever is sitting in that directory.
    staged="$(mktemp -d)"
    trap 'rm -rf "$staged"' EXIT
    for header in "${headers[@]}"; do
      cp "$src/$header" "$staged/"
    done
    if diff -r "$staged" "$crate/include/fdk-aac" >/dev/null 2>&1; then
      echo "   ${#headers[@]} headers, byte-identical"
    else
      echo "the committed headers are not fdk-aac $FDK_AAC_VERSION's — run --headers" >&2
      diff -r "$staged" "$crate/include/fdk-aac" | head -30 >&2
      exit 1
    fi

    diff -q "$src/NOTICE" "$crate/include/NOTICE" >/dev/null || {
      echo "$crate/include/NOTICE is not the pinned tarball's — run --headers" >&2
      exit 1
    }
    diff -q "$src/NOTICE" LICENSE >/dev/null || {
      echo "LICENSE is not the pinned tarball's NOTICE — run --headers" >&2
      exit 1
    }
    echo "   NOTICE and LICENSE match"

    (cd "$crate" && ./gen-version.sh --check && ./gen-bindings.sh --check)
    ;;

  --fetch)
    # For working offline afterwards, or for testing a target this machine cannot build.
    # Takes whatever the latest release holds, which is the same thing build.rs would fetch
    # — including the SHA256SUMS check, because a download verified in one half of this
    # repository and not the other is a difference someone would eventually trip over.
    base="https://github.com/$PREBUILT_REPO/releases/latest/download"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    echo ">> SHA256SUMS"
    curl -sSL --fail --max-time 300 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

    for target in "${targets[@]}"; do
      asset="fdk-aac-${FDK_AAC_VERSION}-${target}.tar.gz"
      echo ">> $asset"
      curl -sSL --fail --max-time 300 -o "$tmp/$asset" "$base/$asset"

      # `./` tolerated on the name for the same reason build.rs tolerates it: how the
      # release job spelled its glob should not be able to break this.
      expected="$(awk -v a="$asset" '$2 == a || $2 == "./" a { print $1 }' "$tmp/SHA256SUMS")"
      [ -n "$expected" ] || { echo "SHA256SUMS does not list $asset" >&2; exit 1; }
      actual="$(sha256_of "$tmp/$asset")"
      [ "$actual" = "$expected" ] || {
        echo "checksum mismatch for $asset" >&2
        echo "  SHA256SUMS says $expected" >&2
        echo "  the download is $actual" >&2
        exit 1
      }

      rm -rf "${prebuilt:?}/${target:?}"
      mkdir -p "$prebuilt/$target"
      tar xzf "$tmp/$asset" -C "$prebuilt/$target"
    done
    ;;

  "")
    # The local loop: whatever ./build.sh has produced becomes what cargo links, with no
    # release and no network in the picture at all.
    [ -d dist ] || { echo "nothing in dist/ — run ./build.sh <target> first" >&2; exit 1; }
    found=0
    for dir in dist/*/; do
      target="$(basename "$dir")"
      [ -f "$dir/MANIFEST" ] || continue
      rm -rf "${prebuilt:?}/${target:?}"
      mkdir -p "$prebuilt"
      cp -R "$dir" "$prebuilt/$target"
      echo ">> $target ($(sed -n 's/^cpu_floor //p' "$dir/MANIFEST"))"
      found=$((found + 1))
    done
    [ "$found" -gt 0 ] || { echo "no built targets in dist/" >&2; exit 1; }
    echo ">> $found target(s) in $prebuilt — cargo will use these before any release"
    ;;

  *)
    sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 1
    ;;
esac
