#!/usr/bin/env bash
# Build one static fdk-aac, with the claims about it verified rather than assumed.
#
# Usage:
#   ./build.sh <target>
#
# Targets:
#   macos-arm64            libfdk-aac.a   (Apple silicon, deployment target 11.0)
#   linux-x86_64           libfdk-aac.a   (x86-64-v3 / Coffee Lake floor)
#   linux-aarch64          libfdk-aac.a   (ARMv8-A baseline)
#   windows-x86_64-msvc    fdk-aac.lib    (x86-64-v3 / Coffee Lake floor, dynamic CRT)
#
# Output: dist/<target>/{lib,include}/… plus a MANIFEST naming the version, the checksum,
# the flags, the CPU floor and — the one this codec needs and libopus did not — which C++
# runtime symbols the archive actually leaves undefined.
#
# **cmake, deliberately.** fdk-aac's own build is what knows which of its fifteen module
# directories are compiled, in what order, with which include paths; `fdk-aac-sys` reproduces
# that as a hand-maintained list of 170 file paths in a build.rs, which is a list that goes
# stale silently. Building here with cmake once is exactly what frees every *consumer* from
# needing a C++ toolchain at all.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=fdk-aac.env
. ./fdk-aac.env
# shellcheck source=source.sh
. ./source.sh

target="${1:-}"
[ -n "$target" ] || {
  sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit 1
}

src="build/fdk-aac-${FDK_AAC_VERSION}"
out="$here/dist/$target"

# ---------------------------------------------------------------- source

# Fetched, checksummed and unpacked by source.sh, which sync-prebuilt.sh also uses to take
# the headers and the version constants from — the checksum gate is one implementation, not
# three.
ensure_source

# ---------------------------------------------------------------- configure

# Common to every target.
#
# The two INSTALL_* switches are off because this archive is not installed into a prefix
# anybody points pkg-config or find_package at: a .pc file naming /usr/local and a cmake
# config module exporting an absolute path are both actively misleading sitting inside a
# relocatable tarball.
cmake_args=(
  -DCMAKE_BUILD_TYPE=Release
  -DBUILD_SHARED_LIBS=OFF
  -DBUILD_PROGRAMS=OFF
  -DFDK_AAC_INSTALL_CMAKE_CONFIG_MODULE=OFF
  -DFDK_AAC_INSTALL_PKGCONFIG_MODULE=OFF
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON
)

# Required, not defensive. fdk-aac declares `cmake_minimum_required(VERSION 3.5.1)`, and
# CMake 4 — which is what a current runner and a current Homebrew both install — refuses
# that outright rather than warning. Without this line the configure step of every target
# fails on the first machine that upgrades.
cmake_args+=(-DCMAKE_POLICY_VERSION_MINIMUM=3.5)

# Make the archive reproducible.
#
# fdk-aac compiles `__DATE__` and `__TIME__` into eight of its modules — `aacEncGetLibInfo`
# reports them as `build_date` and `build_time` — so two builds of the same source produce
# different bytes. Worse than different-per-day: `__TIME__` is evaluated per *translation
# unit*, so a single build embeds a spread of timestamps across however many seconds the
# compile took, and even two runs on one machine in one minute disagree.
#
# `SOURCE_DATE_EPOCH` is the reproducible-builds standard for exactly this, and GCC and
# Clang both honour it by substituting a fixed value for those macros. Pinned to the
# beginning of 1980 rather than 0 because some toolchains reject a pre-epoch or zero
# timestamp, and nothing here reads the value back.
#
# Two things this does not cover, both stated rather than papered over:
#   - **MSVC ignores it.** cl.exe has no SOURCE_DATE_EPOCH support and no way to redefine
#     __DATE__, so `windows-x86_64-msvc` is not reproducible and its archive's hash moves
#     between builds. The CI job that asserts reproducibility builds linux-x86_64 only.
#   - it says nothing about the .tar.gz around the archive, which gzip stamps with an mtime
#     of its own. That is why `sha256(library)` in the MANIFEST is the checksum worth
#     comparing between releases, and the tarball's is only good for catching a bad download.
export SOURCE_DATE_EPOCH=315532800

# Extra compiler flags, per target. Kept separate from `cmake_args` because they are the one
# thing here that decides *which machines the artifact runs on*, and the MANIFEST records
# them for exactly that reason.
#
# Note what these are not: `-march=native`. Nothing may be tuned to the *builder's* CPU,
# because the artifact is linked into binaries that run elsewhere. What each target does
# instead is name a **floor** — the oldest CPU the resulting library is allowed to require —
# and compile for that.
cflags=()

# Does the compiler take this flag? A build that silently ignores `-mcpu=apple-m1` produces
# a working library, just a slower one, which is the failure mode this repo exists to make
# loud.
#
# Probed with the *C++* compiler, unlike libopus-prebuilt's version of this function: every
# file fdk-aac compiles is C++, so `CMAKE_CXX_FLAGS` is where the floor has to land and a
# flag accepted by cc but not by c++ would pass a C probe and then fail the build.
cxxflag_supported() {
  echo 'int main(void){return 0;}' |
    ${CXX:-c++} -Werror "$1" -x c++ - -o /dev/null >/dev/null 2>&1
}

# Overwritten by every target below; the `*)` arm exits rather than falling through, so this
# value never reaches a MANIFEST. It says what it is anyway, because an empty string in a
# MANIFEST would look like a bug in the build rather than an unreachable default.
floor='unset'

lib_name=libfdk-aac.a
case "$target" in
  macos-arm64)
    cmake_args+=(
      -DCMAKE_OSX_ARCHITECTURES=arm64
      # Lower than any consumer targets. A static library built for a *newer* minimum than
      # the binary linking it is a link-time warning today and a support call later; there
      # is no cost to building older.
      -DCMAKE_OSX_DEPLOYMENT_TARGET=11.0
    )
    # Every arm64 Mac is an M1 or later — Apple never shipped another arm64 CPU — so naming
    # the M1 as the floor costs no compatibility at all, and buys the compiler ARMv8.4 with
    # dotprod and fp16 instead of the generic ARMv8-A it assumes otherwise.
    floor='apple-m1 (armv8.4)'
    if cxxflag_supported -mcpu=apple-m1; then
      cflags+=(-mcpu=apple-m1)
    else
      echo "   note: this compiler rejects -mcpu=apple-m1, building generic armv8-a" >&2
      floor='armv8-a (compiler rejected -mcpu=apple-m1)'
    fi
    ;;
  linux-x86_64)
    # The same Coffee Lake floor libopus-prebuilt uses, so that a project linking both gains
    # no exclusion it did not already have.
    #
    # What it buys here is *not* what it buys there. opus ships hand-written SSE4.1 and AVX2
    # kernels, and the flag decides whether they are called. fdk-aac ships no x86 SIMD at
    # all — it is fixed-point integer code, and its hand-written assembly is 32-bit ARM only
    # — so all this can do is let the compiler autovectorize the scalar loops. That is worth
    # having and it is not worth overclaiming, which is why the verification below records
    # the AVX instruction count rather than requiring some number of them.
    #
    # Cost, stated plainly and deliberately accepted: the artifact may execute an illegal
    # instruction on anything without AVX2 — pre-2013 Intel, pre-Zen AMD, and the Pentium
    # and Celeron parts *of* the Coffee Lake generation, where it is fused off. Anything
    # below that floor wants its own build, which FDK_AAC_PREBUILT_DIR is for.
    floor='x86-64-v3 / Coffee Lake (AVX2+FMA permitted)'
    cflags+=(-march=x86-64-v3 -mtune=skylake)
    ;;
  linux-aarch64)
    # No floor to *choose*. arm64 Linux spans a decade of very different cores, NEON is
    # mandatory in ARMv8-A anyway, and fdk-aac has nothing above the baseline to unlock:
    # its `arm/` sources are guarded on `__arm__` and `__ARM_ARCH_8__`, both of which are
    # 32-bit ARM. Naming a higher floor here would cost compatibility for nothing.
    floor='armv8-a (baseline)'
    ;;
  windows-x86_64-msvc)
    lib_name=fdk-aac.lib
    # Rust's MSVC targets link the dynamic CRT, and a static library built against the
    # static one fails to link with the mismatch that costs everybody an afternoon.
    cmake_args+=(-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL)
    # Same Coffee Lake floor as linux-x86_64. `/arch:AVX2` is the whole of what cl.exe
    # offers here — there is no `/arch:` level between AVX2 and AVX512, and no separate
    # tuning flag.
    floor='x86-64-v3 / Coffee Lake (AVX2+FMA permitted)'
    cflags+=(/arch:AVX2)
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

if [ ${#cflags[@]} -gt 0 ]; then
  # CXX, not C. Every source file in fdk-aac is C++ — `LINKER_LANGUAGE C` in its
  # CMakeLists is about how the *library* is linked, not how it is compiled — so
  # CMAKE_C_FLAGS alone would set a floor on the zero C files in the build and leave the
  # archive itself compiled at the default baseline.
  cmake_args+=("-DCMAKE_CXX_FLAGS=${cflags[*]}")
fi

echo ">> configuring fdk-aac ${FDK_AAC_VERSION} for $target"
cmake -S "$src" -B "build/$target" "${cmake_args[@]}"

echo ">> building"
cmake --build "build/$target" --config Release --parallel

# ---------------------------------------------------------------- collect

archive="$(find "build/$target" -name "$lib_name" -type f | head -1)"
[ -n "$archive" ] || {
  echo "no $lib_name was produced" >&2
  exit 1
}

rm -rf "$out"
mkdir -p "$out/lib" "$out/include/fdk-aac"
cp "$archive" "$out/lib/$lib_name"
# The six public headers, taken from the same tree this archive was compiled from — a
# consumer generating its own bindings must not be reading a different version's.
for header in libAACenc/include/aacenc_lib.h libAACdec/include/aacdecoder_lib.h \
              libSYS/include/FDK_audio.h libSYS/include/genericStds.h \
              libSYS/include/machine_type.h libSYS/include/syslib_channelMapDescr.h; do
  cp "$src/$header" "$out/include/fdk-aac/"
done
# Fraunhofer's licence, next to Fraunhofer's headers, from the same verified tarball.
# Shipping somebody's source without their licence text is not a thing to do by omission,
# and this licence in particular is one a consumer has to have read.
cp "$src/NOTICE" "$out/include/"

# ---------------------------------------------------------------- verify

# libopus-prebuilt asserts here that the SIMD objects made it into the archive, because a
# scalar opus is indistinguishable from a good one except in speed. That check does not
# transfer: fdk-aac has no x86 SIMD to lose and its ARM assembly is 32-bit only, so grepping
# for kernels would be a check that passes by describing the world correctly rather than by
# testing anything. What is worth asserting instead is below.

echo ">> verifying the entry points are in the archive"
# The functions the crates above actually call. An archive that configured itself down to
# the decoder only, or that landed under the right name with the wrong contents, fails here
# rather than at the link step of every consumer.
entry_points='aacEncOpen aacEncEncode aacEncInfo aacEncGetLibInfo aacEncoder_SetParam
              aacDecoder_Open aacDecoder_DecodeFrame aacDecoder_GetStreamInfo aacDecoder_Fill'

case "$target" in
  windows-*)
    # `nm` is not on a Windows runner's PATH and `lib /list` needs an MSVC environment this
    # script does not set up. The compiled object files answer a weaker version of the same
    # question one step earlier: if cmake decided against a module, its objects do not exist.
    for module in libAACenc libAACdec libFDK libMpegTPEnc libMpegTPDec libSBRenc libSBRdec; do
      found="$(find "build/$target" -path "*$module*" -name '*.obj' | wc -l | tr -d ' ')"
      [ "$found" -gt 0 ] || {
        echo "no object files for $module — the build is missing a module" >&2
        exit 1
      }
    done
    echo "   every module produced objects"
    ;;
  *)
    symbols="$(nm --defined-only "$out/lib/$lib_name" 2>/dev/null || true)"
    for symbol in $entry_points; do
      # A here-string rather than `printf … | grep -q`: under `set -o pipefail`, grep -q
      # exits on the first match, the writer takes SIGPIPE, and the pipeline reports 141 —
      # so a *found* symbol reads as a missing one. Cost an hour once; not twice.
      #
      # `[ _]` because Mach-O prefixes every C symbol with an underscore and ELF does not.
      grep -qE "[ _]${symbol}$" <<<"$symbols" || {
        echo "$symbol is not defined in $lib_name — this is not a complete fdk-aac" >&2
        exit 1
      }
    done
    echo "   $(printf '%s\n' "$entry_points" | wc -w | tr -d ' ') entry points defined"
    ;;
esac

# Which C++ runtime this archive needs — measured, not assumed.
#
# `fdk-aac-sys` emits `-lstdc++` on Linux and `-lc++` on macOS unconditionally, and every
# consumer of it therefore acquires a dependency on the C++ runtime whether or not one is
# required. It is worth knowing which, because "worth linking anyway" and "must be linked"
# differ on a slim runtime image.
#
# The reason to expect none: cmake compiles this with `-fno-exceptions -fno-rtti` and sets
# `LINKER_LANGUAGE C`, and the source uses no `new`, no `delete`, no STL and no virtual
# functions — it is C written in C++ files. But that is a claim about the source, and the
# thing that decides is the archive, so the archive is what gets asked.
echo ">> measuring the C++ runtime requirement"
cxx_runtime='not measured (no nm on this platform)'
case "$target" in
  windows-*) ;;
  *)
    # Undefined symbols matching the C++ ABI: Itanium mangling for operator new/delete
    # (_Znw*/_Zna*/_Zdl*/_Zda*), the std:: namespace (_ZSt*, _ZNSt*), and the runtime
    # helpers a compiler emits by itself (__cxa_*, __gxx_personality_*, _Unwind_*).
    cxx_undefined="$(nm --undefined-only "$out/lib/$lib_name" 2>/dev/null |
      awk '{print $NF}' |
      grep -E '^_?(_Zn[wa]|_Zd[la]|_ZN?St|__cxa_|__gxx_personality|_Unwind_)' |
      sort -u || true)"
    if [ -z "$cxx_undefined" ]; then
      cxx_runtime='none'
      echo "   none — the archive needs no libstdc++/libc++"
    else
      # Not a failure. It is a fact the MANIFEST has to carry, because build.rs reads this
      # line to decide whether to emit the link flag, and check-static.sh reads it to decide
      # whether a C++ runtime dependency in a finished binary is expected or a regression.
      cxx_runtime="required: $(printf '%s' "$cxx_undefined" | tr '\n' ' ' | sed 's/ $//')"
      echo "   required:"
      # Read line by line rather than letting word splitting break the list apart: the
      # symbols are already newline-separated, and an unquoted expansion would also glob.
      while IFS= read -r symbol; do
        echo "     $symbol"
      done <<<"$cxx_undefined"
    fi
    ;;
esac

# And that the floor this target claims is the floor it was actually built with. A flag
# CMake accepted into its cache but the compiler silently ignored produces a *working*
# library wearing a MANIFEST that lies about which machines it runs on.
echo ">> verifying the CPU floor"
if [ ${#cflags[@]} -gt 0 ]; then
  cache="build/$target/CMakeCache.txt"
  for flag in "${cflags[@]}"; do
    grep -q -- "^CMAKE_CXX_FLAGS:STRING=.*${flag}" "$cache" || {
      echo "$flag is not in CMAKE_CXX_FLAGS in the cache — the floor did not take" >&2
      exit 1
    }
  done
  echo "   ${cflags[*]} reached the compiler"
fi

# Evidence rather than a gate, and the distinction is the honest part. With no hand-written
# x86 kernels in this library, the AVX instruction count is whatever the autovectorizer
# chose to emit; requiring some number of them would be asserting a property nothing
# guarantees. Recording it means a bump that quietly loses vectorization is visible by
# comparing two MANIFESTs, which is what the number is actually good for.
avx_evidence='n/a'
case "$target" in
  linux-x86_64 | macos-arm64 | linux-aarch64)
    if command -v objdump >/dev/null 2>&1; then
      case "$target" in
        linux-x86_64)
          count="$(objdump -d "$out/lib/$lib_name" 2>/dev/null |
            grep -ciE '\s(vp[a-z]+|vmov[a-z]*|vfmadd[0-9a-z]*)\s' || true)"
          avx_evidence="${count:-0} AVX/AVX2 instructions"
          ;;
        *)
          count="$(objdump -d "$out/lib/$lib_name" 2>/dev/null |
            grep -ciE '\s(ld[0-9]|st[0-9]|fmla|smlal|sqdmulh)\s' || true)"
          avx_evidence="${count:-0} NEON instructions"
          ;;
      esac
      echo "   $avx_evidence"
    fi
    ;;
esac
echo "   floor: $floor"

cflags_note='(none)'
[ ${#cflags[@]} -eq 0 ] || cflags_note="${cflags[*]}"

echo ">> checksumming the archive"
# The library's own hash, not the tarball's. A .tar.gz is not reproducible — gzip stamps an
# mtime into its header — so the wrapper's checksum can only ever say "these are the bytes
# that were published". This one says something stronger and more useful: *this is the same
# library*, comparable across runs, machines and releases.
lib_sha="$(sha256_of "$out/lib/$lib_name")"
echo "   $lib_sha"

{
  echo "fdk-aac $FDK_AAC_VERSION"
  echo "target $target"
  echo "sha256(source) $FDK_AAC_SHA256"
  echo "sha256(library) $lib_sha"
  echo "library lib/$lib_name"
  echo "cpu_floor $floor"
  echo "cxx_runtime $cxx_runtime"
  echo "simd_evidence $avx_evidence"
  echo "cflags $cflags_note"
  echo "cmake_args ${cmake_args[*]}"
} > "$out/MANIFEST"

echo ">> wrote $out"
cat "$out/MANIFEST"
