#!/usr/bin/env bash
# Assert that a binary carries fdk-aac inside it rather than expecting to find one.
#
#   ./check-static.sh target/release/fdk-aac-e2e [target]
#
# `target` names the archive the binary linked — `linux-x86_64-v3` for a build with the
# x86-64-v3 feature, say — and defaults to this machine's baseline target. Its MANIFEST, and no
# other, is what the C++ runtime check below reads.
#
# Three questions, and they fail in different directions:
#
#   positive — is fdk-aac actually *in* there? The library titles its own modules ("AAC
#             Encoder", "AAC Decoder Lib") and those strings are compiled into the archive,
#             so finding them in the file's bytes says yes. fdk-aac compiles no version
#             string, unlike libopus, so there is nothing more specific to grep for — the
#             version assertion is the e2e binary's job, at runtime, against the generated
#             constants.
#   negative — is there a *dynamic* dependency on fdk-aac as well or instead? This is the
#             one that passes every test on the build machine and then fails on a slim
#             runtime image or a machine without Homebrew, which is precisely the failure
#             this repository exists to remove.
#   the C++ runtime — build.sh measures whether the archive needs one and records the answer
#             in the MANIFEST. On every target so far the answer is "none", which is better
#             than upstream fdk-aac-sys manages: it emits -lstdc++ unconditionally. That is
#             a property worth keeping, so when the MANIFEST says `none` this requires the
#             finished binary to have no libstdc++/libc++ dependency either.
#
# Run in CI on every target. A binary that links a system fdk-aac by accident behaves
# identically to a correct one until it is copied somewhere else.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

bin="${1:?usage: ./check-static.sh <binary> [target]}"
[ -f "$bin" ] || { echo "no such file: $bin" >&2; exit 1; }

# The archive the binary linked: named by the caller, or else this machine's baseline target.
# Only the caller can know about the -v3 flavours, which a cargo feature selects.
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) host_target=macos-arm64 ;;
  Linux-x86_64) host_target=linux-x86_64 ;;
  Linux-aarch64 | Linux-arm64) host_target=linux-aarch64 ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) host_target=windows-x86_64-msvc ;;
  *) host_target=unknown ;;
esac
target="${2:-$host_target}"

# shellcheck source=fdk-aac.env
. "$here/fdk-aac.env"

fail=0

echo ">> $bin"

# `grep -a`: treat the executable as text. Portable to all three platforms, which `nm` and
# `strings` are not — Windows runners have neither.
for marker in "AAC Encoder" "AAC Decoder Lib"; do
  if grep -aq "$marker" "$bin"; then
    echo "   ok    '$marker' is compiled in"
  else
    echo "   FAIL  no '$marker' string in the binary — is fdk-aac really linked?" >&2
    fail=1
  fi
done

case "$(uname -s)" in
  Darwin) deps="$(otool -L "$bin" | tail -n +2 || true)" ;;
  Linux)  deps="$(ldd "$bin" 2>/dev/null || true)" ;;
  *)
    # Windows, under Git Bash. `dumpbin /dependents` needs an MSVC environment this script
    # does not set up, so the PE import table is read the crude way: a DLL a binary imports
    # has its name stored, in ASCII, in the file.
    deps="$(grep -aoiE '[a-z0-9_.+-]*\.dll' "$bin" | sort -u || true)"
    ;;
esac

if aac_deps="$(printf '%s\n' "$deps" | grep -iE 'fdk[-_]?aac')" && [ -n "$aac_deps" ]; then
  echo "   FAIL  dynamic dependency on fdk-aac:" >&2
  printf '           %s\n' "$aac_deps" >&2
  fail=1
else
  echo "   ok    no dynamic fdk-aac dependency"
fi

# What did build.sh measure for this target? Looked up rather than assumed, and skipped
# rather than guessed when there is no MANIFEST to read — a check that invents its own
# expectation is worse than one that says it did not run. **This** target's, by name: taking
# the last MANIFEST the globs found reported another target's measurement — the linux-x86_64
# container run read windows-x86_64-msvc's from a prebuilt/ cache the tree carried (the same bug
# libvpx-prebuilt fixed).
manifest=""
for candidate in "$here/dist/$target/MANIFEST" \
                 "$here/crates/fdk-aac-prebuilt-sys/prebuilt/$target/MANIFEST"; do
  if [ -f "$candidate" ]; then
    manifest="$candidate"
    break
  fi
done

if [ -n "$manifest" ]; then
  cxx_runtime="$(sed -n 's/^cxx_runtime //p' "$manifest")"
  echo "   note  $(basename "$(dirname "$manifest")") MANIFEST says cxx_runtime: ${cxx_runtime:-<absent>}"
  if [ "$cxx_runtime" = "none" ]; then
    if cxx_deps="$(printf '%s\n' "$deps" | grep -iE 'libstdc\+\+|libc\+\+')" && [ -n "$cxx_deps" ]; then
      echo "   FAIL  the archive needs no C++ runtime, but the binary links one:" >&2
      printf '           %s\n' "$cxx_deps" >&2
      echo "         Something re-added an unconditional -lstdc++/-lc++. See" >&2
      echo "         link_cxx_runtime() in crates/fdk-aac-prebuilt-sys/build.rs." >&2
      fail=1
    else
      echo "   ok    no C++ runtime dependency, as the measurement predicted"
    fi
  fi
else
  echo "   note  no $target MANIFEST found — skipping the C++ runtime check"
fi

exit "$fail"
