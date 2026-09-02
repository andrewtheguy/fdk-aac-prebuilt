#!/usr/bin/env bash
# Build one static fdk-aac, with the claims about it verified rather than assumed.
#
# Usage:
#   ./build.sh <target>
#
# Targets:
#   macos-arm64            libfdk-aac.a   (Apple silicon, deployment target 11.0)
#   linux-x86_64           libfdk-aac.a   (x86-64 baseline; no floor above SSE2)
#   linux-x86_64-v3        libfdk-aac.a   (x86-64-v3 / Coffee Lake floor: AVX2 autovectorized)
#   linux-aarch64          libfdk-aac.a   (ARMv8-A baseline)
#   windows-x86_64-msvc    fdk-aac.lib    (x86-64 baseline; dynamic CRT)
#   windows-x86_64-msvc-v3 fdk-aac.lib    (x86-64-v3 / Coffee Lake floor; dynamic CRT)
#
# The `-v3` targets are the same configuration as their base target plus a CPU floor. Both
# flavours are published on every release; the crate links the baseline unless a consumer
# enables its `x86-64-v3` feature. See the note under linux-x86_64 below for why the choice
# has to be made at build time rather than at run time.
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
# and compile for that. On x86_64 that floor is the baseline itself, for the reason given
# under linux-x86_64 below.
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

# Which flavour of an x86_64 target this is. `baseline` is every target that has no `-v3`
# suffix, including the arm ones, for which the word means "the floor this target names";
# the verification below only consults it on x86_64.
flavour=baseline
case "$target" in *-v3) flavour=v3 ;; esac

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
  linux-x86_64 | linux-x86_64-v3)
    # The default archive has no floor above the x86-64 baseline, and no `-march`. It was
    # `-march=x86-64-v3 -mtune=skylake` — the Coffee Lake floor libopus-prebuilt had at the
    # time — until a binary linking that repository's archive died on the first AVX2
    # instruction on an Ivy Bridge, and both repositories dropped the floor.
    #
    # libopus-prebuilt could drop it and keep its SIMD, because opus compiles SSE4.1 and AVX2
    # kernels per file and picks one by CPUID at run time. fdk-aac has no such mechanism to
    # keep. There is no CPUID query, no function multiversioning and no SIMD kernel anywhere
    # in it: the x86 headers hold a scalar `imul` and four float-based math routines, the arm
    # headers hold scalar inline assembly, and that is the whole of its per-architecture
    # code. So "dispatch or presume" is not a choice this library offers; the only knob is
    # the floor, and the only thing the floor changes is which instructions the
    # autovectorizer may use for the scalar loops. At the baseline it still vectorizes them
    # with SSE2, which the verification below counts.
    #
    # Since the choice cannot be made at run time, it is made at build time, twice: the
    # `-v3` flavour is the same archive with the old floor back, for consumers who have
    # measured the difference (the README has the numbers: five to ten percent of about one
    # percent of a core) and know every machine they ship to has AVX2. The crate picks it
    # with a cargo feature; the default is the archive that cannot SIGILL.
    #
    # `-mtune=skylake` on the v3 flavour changes scheduling only, never the instruction set,
    # and is kept so the archive is byte-identical to the releases that shipped before the
    # floor was dropped — the reproducibility comparison across releases depends on that.
    if [ "$flavour" = v3 ]; then
      floor='x86-64-v3 / Coffee Lake (AVX2+FMA unconditional; fdk-aac has no runtime dispatch)'
      cflags+=(-march=x86-64-v3 -mtune=skylake)
    else
      floor='x86-64 baseline (SSE2; fdk-aac has no runtime dispatch, so nothing above it is used)'
    fi
    ;;
  linux-aarch64)
    # No floor to *choose*. arm64 Linux spans a decade of very different cores, NEON is
    # mandatory in ARMv8-A anyway, and fdk-aac has nothing above the baseline to unlock. Its
    # `arm/` headers *are* active here — FDK_archdef.h defines `__arm__` and `__ARM_ARCH_8__`
    # from `__aarch64__`, and under those the headers carry A64 inline assembly for the
    # fixed-point multiplies, `cplxMultDiv2` and `clz` (an earlier version of this comment
    # said they were 32-bit only) — but every one of those instructions is baseline ARMv8-A
    # scalar code, so a higher floor would cost compatibility for nothing.
    floor='armv8-a (baseline)'
    ;;
  windows-x86_64-msvc | windows-x86_64-msvc-v3)
    lib_name=fdk-aac.lib
    # Rust's MSVC targets link the dynamic CRT, and a static library built against the
    # static one fails to link with the mismatch that costs everybody an afternoon.
    cmake_args+=(-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL)
    # Same two flavours as Linux. cl.exe's x64 default code generation is SSE2, which is the
    # baseline, and it autovectorizes at that level as GCC does; `/arch:AVX2` is the whole
    # of what it offers for the v3 flavour — there is no `/arch:` level between AVX2 and
    # AVX512, and no separate tuning flag.
    if [ "$flavour" = v3 ]; then
      floor='x86-64-v3 / Coffee Lake (AVX2+FMA unconditional; fdk-aac has no runtime dispatch)'
      cflags+=(/arch:AVX2)
    else
      floor='x86-64 baseline (SSE2; fdk-aac has no runtime dispatch, so nothing above it is used)'
    fi
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

# CXX, not C. Every source file in fdk-aac is C++ — `LINKER_LANGUAGE C` in its CMakeLists is
# about how the *library* is linked, not how it is compiled — so CMAKE_C_FLAGS alone would
# set a floor on the zero C files in the build and leave the archive itself compiled at the
# default baseline.
#
# Passed even when empty, on purpose. An explicit `-DCMAKE_CXX_FLAGS=` is what stops cmake
# seeding the variable from a `CXXFLAGS` in the runner's environment — the one route by
# which `-march=native` could reach this archive from outside the script.
cmake_args+=("-DCMAKE_CXX_FLAGS=${cflags[*]}")

# From a clean build directory, always. The source tree is re-unpacked on every run (see
# source.sh), so every object would be recompiled anyway; what a stale directory keeps is the
# *cache*, and a cache remembers flags. The first baseline build of linux-x86_64 on a machine
# that had built the x86-64-v3 archive found `-march=x86-64-v3` still in CMakeCache.txt,
# reported `[100%] Built target` without compiling a file, and would have shipped the AVX2
# archive under a MANIFEST saying baseline — the floor check below is what caught it.
rm -rf "build/$target"

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
# transfer: fdk-aac has no SIMD kernels to lose on any architecture — its per-architecture
# code is scalar inline assembly and a few float-based routines, all in headers, all inlined —
# so grepping for kernels would be a check that passes by describing the world correctly
# rather than by testing anything. What is worth asserting instead is below, and on x86_64
# it is the opposite property: that nothing *above* the baseline got in.

echo ">> verifying the entry points are in the archive"
# The functions the crates above actually call. An archive that configured itself down to
# the decoder only, or that landed under the right name with the wrong contents, fails here
# rather than at the link step of every consumer.
entry_points='aacEncOpen aacEncEncode aacEncInfo aacEncGetLibInfo aacEncoder_SetParam
              aacDecoder_Open aacDecoder_DecodeFrame aacDecoder_GetStreamInfo aacDecoder_Fill'

case "$target" in
  windows-*)
    # `nm` is not on a Windows runner's PATH and `lib /list` needs an MSVC environment this
    # script does not set up. So the archive's own index is read instead.
    #
    # A COFF archive's **first linker member** is exactly that index: the symbols the archive
    # *defines*. That is the same question `nm --defined-only` asks in the branch below, and
    # the reason it has to be this rather than a scan of the file's printable strings — which
    # is what this did first. A string scan also matches *undefined* externals, and on this
    # archive `memcpy` gets 14 such hits while being imported from the CRT rather than defined
    # here. An entry point that had gone missing but was still referenced would have passed.
    #
    # Layout, from the PE/COFF specification: the 8-byte `!<arch>\n` magic, a 60-byte member
    # header whose name is `/` and whose 10-byte size field sits at offset 48, then the member
    # — a 4-byte **big-endian** count, that many 4-byte offsets, then that many NUL-terminated
    # names. Read with `dd` rather than piped through `head`, because `head` exiting early
    # takes the writer down with SIGPIPE and `set -o pipefail` then reports 141; see the note
    # in the other branch, which is the same trap.
    lib_path="$out/lib/$lib_name"
    [ "$(dd if="$lib_path" bs=1 count=8 2>/dev/null)" = '!<arch>' ] || {
      echo "$lib_name is not an archive — its magic is not '!<arch>'" >&2
      exit 1
    }
    member_name="$(dd if="$lib_path" bs=1 skip=8 count=16 2>/dev/null)"
    [ "${member_name%%[[:space:]]*}" = '/' ] || {
      echo "$lib_name has no first linker member — cannot list its defined symbols" >&2
      exit 1
    }
    member_size="$(dd if="$lib_path" bs=1 skip=56 count=10 2>/dev/null | tr -d '[:space:]')"
    symbol_count=$((16#$(od -An -tx1 -N4 -j 68 "$lib_path" | tr -d ' \n')))
    # The names run from just past the offset array to the end of the member.
    names_at=$((72 + symbol_count * 4))
    names_len=$((member_size - 4 - symbol_count * 4))
    if [ "$symbol_count" -le 0 ] || [ "$names_len" -le 0 ]; then
      echo "$lib_name's linker member lists no symbols — this is not a complete fdk-aac" >&2
      exit 1
    fi
    symbols="$(dd if="$lib_path" bs=1 skip="$names_at" count="$names_len" 2>/dev/null |
      LC_ALL=C tr '\0' '\n')"

    # x64 is why the names match literally: MSVC decorates `__cdecl` symbols with a leading
    # underscore on x86 but not on x64, and these are plain C entry points.
    for symbol in $entry_points; do
      grep -qxF "$symbol" <<<"$symbols" || {
        echo "$symbol is not defined in $lib_name — this is not a complete fdk-aac" >&2
        exit 1
      }
    done
    echo "   $(printf '%s\n' "$entry_points" | wc -w | tr -d ' ') entry points defined" \
         "(of $symbol_count in the archive index)"
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

# And that the floor this target claims is the floor it was actually built with. Two ways to
# get this wrong, and they fail in opposite directions. A flag CMake accepted into its cache
# but the compiler silently ignored produces a *working* library wearing a MANIFEST that lies
# about which machines it runs on — slower than claimed. A flag that reached the compiler
# without being asked for — cmake seeds CMAKE_CXX_FLAGS from a `CXXFLAGS` in the environment,
# so a runner with `-march=native` exported there would floor this archive from an unchanged
# script — produces a library that SIGILLs on somebody's machine while its MANIFEST says
# baseline. The second is the one libopus-prebuilt shipped, so it is checked harder.
echo ">> verifying the CPU floor"
cache="build/$target/CMakeCache.txt"
cached_cxx_flags="$(sed -n 's/^CMAKE_CXX_FLAGS:STRING=//p' "$cache")"
if [ ${#cflags[@]} -gt 0 ]; then
  for flag in "${cflags[@]}"; do
    grep -q -- "^CMAKE_CXX_FLAGS:STRING=.*${flag}" "$cache" || {
      echo "$flag is not in CMAKE_CXX_FLAGS in the cache — the floor did not take" >&2
      exit 1
    }
  done
  echo "   ${cflags[*]} reached the compiler"
fi
case "$target:$flavour" in
  *x86_64*:baseline)
    # The cache proves intent: nothing that names a CPU or an instruction set extension, from
    # this script or from the environment. `-mtune` is included although it cannot break a
    # machine, because a CPU name in the MANIFEST of an archive built for every CPU reads as
    # a floor to whoever is debugging the next SIGILL.
    if grep -qE -- '(^|[[:space:]])(-march=|-mcpu=|-mtune=|-m(sse|avx|fma)|/arch:)' <<<"$cached_cxx_flags"; then
      echo "CMAKE_CXX_FLAGS in the cache names a CPU: '$cached_cxx_flags' — a floor leaked in" >&2
      exit 1
    fi
    echo "   no -march, -mcpu, -mtune or /arch: in CMAKE_CXX_FLAGS"
    ;;
  *x86_64*:v3)
    # The opposite failure: a v3 archive that quietly came out at the baseline is merely
    # slower, but it wears a MANIFEST claiming a floor it does not have, and the loop above
    # only proved the flags are in the cache. The disassembly check below proves the
    # compiler acted on them, where a disassembler exists.
    [ ${#cflags[@]} -gt 0 ] || {
      echo "the v3 flavour of $target set no compiler flags — this is the script's bug" >&2
      exit 1
    }
    ;;
esac

# The archive proves the result, where a disassembler that understands the format exists.
# On the x86_64 baseline the assertion is that no AVX instruction is in it at all — not
# "outside the kernels", as libopus-prebuilt checks, because fdk-aac has no kernels: with no
# runtime dispatch, one AVX instruction anywhere is one machine it will not run on. On the
# v3 flavour it is the reverse: AVX instructions must be present, or the floor did nothing.
# Every AVX mnemonic is VEX-encoded and objdump spells every VEX-encoded mnemonic with a
# leading `v`, so the test is the mnemonic column, with the handful of non-AVX `v*` opcodes
# (the VMX instructions and `verr`/`verw`, none of which a codec emits) excluded by name
# rather than by pattern. Alongside it, the SSE2 packed-integer count is recorded as
# evidence — not a gate — that the autovectorizer is still doing its job at the baseline; on
# the aarch64 targets the NEON count plays the same role.
#
# Windows has no such disassembler on the runner, and the runner's own CPU has AVX2, so
# running the e2e binary there cannot catch a leak either. The cache check above is what
# covers that target, which is stated in its MANIFEST rather than hidden.
avx_evidence='n/a'
case "$target" in
  linux-x86_64 | linux-x86_64-v3 | macos-arm64 | linux-aarch64)
    if command -v objdump >/dev/null 2>&1; then
      # Disassembled once, and *checked by counting instructions*, because the failure that
      # matters here is silent. An objdump built for another architecture still prints file
      # headers and still exits 0 — it writes `can't disassemble for architecture UNKNOWN!`
      # to stderr, which `2>/dev/null` swallows — so it yields several kilobytes of output
      # containing no instructions at all. Testing that the output is merely non-empty is not
      # enough; it passes on exactly that. So the test is whether anything was actually
      # disassembled, and a zero is only reported when there was something to count it in.
      #
      # The alternative, "0 instructions", reads as lost vectorization when it means a blind
      # instrument — and a clean report from a blind instrument is the one thing the rest of
      # this repository refuses to ship.
      disassembly="$(objdump -d "$out/lib/$lib_name" 2>/dev/null || true)"
      # Both objdumps prefix a disassembled line with its address and a colon.
      instructions="$(grep -cE '^[[:space:]]*[0-9a-f]+:' <<<"$disassembly" || true)"
      # These archives hold hundreds of thousands of instructions, so this separates "it
      # worked" from "it did not" with no risk of landing in between.
      if [ "${instructions:-0}" -lt 1000 ]; then
        avx_evidence="not measured (objdump disassembled only ${instructions:-0} instructions)"
        echo "   $avx_evidence"
      else
        # Matched on the *architecture* rather than on the exact target, so that adding
        # `macos-x86_64` to the list above cannot silently count NEON mnemonics in an x86
        # disassembly. The `*)` arm says so rather than guessing, for the same reason the
        # instruction-count test above exists: a number nobody measured is worse than none.
        #
        # `[[:space:]]` rather than `\s` throughout — `\s` is a GNU extension that POSIX ERE
        # does not define, and one of these two patterns runs against BSD grep on the macOS
        # runner.
        case "$target" in
          *-x86_64 | *-x86_64-*)
            # GNU objdump's lines are tab-separated: address, bytes, then mnemonic and
            # operands. Prefix words (`lock`, `rep`, `notrack`, `data16`) land in the
            # mnemonic slot and none of them starts with `v`, so they cannot be miscounted.
            avx="$(awk -F'\t' 'NF >= 3 {
                split($3, f, " "); m = f[1]
                if (m ~ /^v[a-z]/ && m !~ /^(verr|verw|vmcall|vmclear|vmfunc|vmlaunch|vmload|vmmcall|vmptrld|vmptrst|vmread|vmresume|vmrun|vmsave|vmwrite|vmxoff|vmxon)$/) print m
              }' <<<"$disassembly" | sort | uniq -c | sort -rn)"
            avx_count="$(awk '{ n += $1 } END { print n + 0 }' <<<"$avx")"
            if [ "$flavour" = baseline ]; then
              [ "$avx_count" -eq 0 ] || {
                echo "AVX instructions in $lib_name — a floor leaked in. The archive would SIGILL" >&2
                echo "on any CPU without them; by mnemonic:" >&2
                echo "$avx" | sed 's/^/    /' >&2
                exit 1
              }
              sse2="$(grep -cE '[[:space:]](p(add|sub|mul|madd|and|or|xor|unpck|shuf|srl|sll|sra|cmp|max|min)[a-z]*|movdq[au])[[:space:]]' \
                <<<"$disassembly" || true)"
              avx_evidence="0 AVX instructions (asserted), ${sse2:-0} SSE2 packed-integer instructions"
            else
              [ "$avx_count" -gt 0 ] || {
                echo "no AVX instructions in $lib_name despite ${cflags[*]} — the floor did not take" >&2
                exit 1
              }
              avx_evidence="$avx_count AVX instructions (asserted present)"
            fi
            ;;
          *-arm64 | *-aarch64)
            # A dot *or* whitespace after the mnemonic, because the two objdumps disagree on
            # how to print a vector arrangement: Apple's llvm-objdump writes `ld1.4s` and
            # `smlal.4s`, GNU objdump on Linux writes `ld1 {v0.4s}, [x0]`. Requiring
            # whitespace reported `0 NEON instructions` for macos-arm64 — built with
            # `-mcpu=apple-m1` — while linux-aarch64, built with no floor at all, reported
            # 274. The archive was fine; the pattern was measuring one toolchain's syntax.
            count="$(grep -ciE '[[:space:]](ld[0-9]|st[0-9]|fmla|smlal|sqdmulh)[[:space:].]' \
              <<<"$disassembly" || true)"
            avx_evidence="${count:-0} NEON instructions"
            ;;
          *)
            avx_evidence='not measured (no instruction pattern for this architecture)'
            ;;
        esac
        echo "   $avx_evidence"
      fi
    fi
    ;;
  windows-*)
    avx_evidence='not measured (no disassembler on the runner; the cmake cache check above is the evidence)'
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
