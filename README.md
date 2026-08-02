# fdk-aac-prebuilt

Static **Fraunhofer FDK AAC 2.0.3**, built once with cmake so that nothing which *links* it
needs a C++ toolchain at all — plus a Rust crate that replaces `fdk-aac` + `fdk-aac-sys` and
links it.

| target | library | CPU floor |
|---|---|---|
| `macos-arm64` | `libfdk-aac.a` | `apple-m1`, deployment target 11.0 |
| `linux-x86_64` | `libfdk-aac.a` | **x86-64-v3 / Coffee Lake** |
| `linux-aarch64` | `libfdk-aac.a` | ARMv8-A baseline |
| `windows-x86_64-msvc` | `fdk-aac.lib` | **`/arch:AVX2` / Coffee Lake**, dynamic CRT |

## Why

`fdk-aac-sys` 0.5 vendors the entire FDK AAC C++ tree inside the crate and compiles about
170 `.cpp` files with the `cc` crate on **every clean build** — in every CI job, every Docker
layer, every fresh clone. This repository does that compile once, in a pipeline, and
publishes the archives; a consumer's build script then finds one and emits two link flags.

Three things fall out of doing it that way, and the third was a surprise:

- no C++ compiler, no cmake, no autotools in any consuming project;
- the archives are checked — fixed-point code compiled to a stated CPU floor, reproducible
  across runners (on the toolchains that can be, see below), and byte-identical in what they
  *encode* across all four targets (see **Bit-exactness** below);
- **no C++ runtime dependency either.** `build.sh` inspects each archive's undefined symbols
  and finds none from the C++ ABI — fdk-aac is C written in `.cpp` files, and cmake builds it
  with `-fno-exceptions -fno-rtti` — so `build.rs` emits no `-lstdc++`. `fdk-aac-sys` emits
  one unconditionally. A binary linking this has no libstdc++ dependency at all.

## Using it from Rust

One line in your manifest:

```toml
# was: fdk-aac = "0.8"
fdk-aac = { package = "fdk-aac-prebuilt", git = "https://github.com/andrewtheguy/fdk-aac-prebuilt", tag = "v2.0.3-…" }
```

(Use a tag that exists — see **Releasing**. The tag pins the *crate*; the archives always come
from the repository's latest release.)

`fdk-aac-prebuilt` sets `[lib] name = "fdk_aac"`, so every `use fdk_aac::enc::…` and
`fdk_aac::dec::Decoder::new` keeps compiling. No cmake, no C++ compiler, no `FDK_AAC_*`
environment variables anywhere — not in a Dockerfile, not in a packaging script, not in CI.

Three crates are involved:

| crate | what it is |
|---|---|
| `fdk-aac-prebuilt-sys` | the FFI, and a build script that finds the right archive and emits the link flags |
| `fdk-aac-prebuilt` | the safe API — `fdk-aac` 0.8.0 by Hailey Somerville, with the fixes below |
| `fdk-aac-e2e` | not a library: a consumer, written with the same dependency line you would use, built and *run* by the pipeline on all four targets |

### What this fork changed, and why

The sibling [`libopus-prebuilt`](https://github.com/andrewtheguy/libopus-prebuilt) keeps its
safe wrapper byte-identical to upstream `opus`, deliberately. This one does not, because
upstream `fdk-aac` 0.8.0 has defects a repository whose point is a *trustworthy* AAC build
should not ship unfixed.

| change | why |
|---|---|
| **`Encoder::encode` takes `&mut self`**, and `Encoder` is no longer `Sync` | upstream had `encode(&self)` plus `unsafe impl Sync`, so two threads sharing an `&Encoder` was a data race reachable from entirely safe code. `aacEncEncode` rewrites the bit reservoir, the psychoacoustic history and the framer's counters. |
| **HE-AAC works** | upstream set `AACENC_SBR_MODE` to 0 on every encoder, with the comment "hardcode SBR off for now". Zero does not mean "leave the default alone" — it *disables* Spectral Band Replication. `Mpeg4HeAac` and `Mpeg4HeAacV2` signalled HE-AAC and produced plain AAC-LC at whatever bitrate they were given. |
| `Encoder::flush` | upstream had no way to drain the encoder, so the last ~2048 samples of every stream it produced were lost — a truncated tail on every file. |
| LOAS, LATM and ADIF transports, on both sides | upstream had ADTS and raw only, written as bare `0` and `2` rather than the `TRANSPORT_TYPE` constants. |
| `set_param` / `get_param` on both encoder and decoder | upstream exposed no parameter access, so the afterburner, the bandwidth, the signalling mode and the decoder's concealment strategy were all unreachable. |
| `Decoder::new` returns `Result` | `aacDecoder_Open` returns NULL on failure; upstream stored the NULL and returned `Self`, so it surfaced later as an unrelated error or as a safe function handing out a reference to address zero. |
| `EncoderError`/`DecoderError` implement `std::error::Error`; `DecoderError::is_concealed()` | `?` into a boxed error now works, and the concealed-vs-fatal distinction — which decides whether a consumer drops a stream or keeps playing — is no longer a hand-maintained list of constants at every call site. |
| `EncoderParams: Default + Copy` | so `EncoderParams { channels: Mono, ..Default::default() }` works. |

**The one source change a caller may need** is `let mut encoder` instead of `let encoder`.

Everything else — the error types, their message tables, `EncodeInfo`, `InfoStruct`,
`StreamInfo`, the module layout — is upstream's and stays that way.

### Where the archive comes from

`build.rs` looks in three places, in order:

1. `FDK_AAC_PREBUILT_DIR` — a prefix containing `lib/`, used as-is. The escape hatch for an
   unsupported target, and the way to build with no network whatsoever.
2. `crates/fdk-aac-prebuilt-sys/prebuilt/<target>/` — what `./build.sh` + `./sync-prebuilt.sh`
   leave behind locally. Gitignored.
3. the repository's **latest** GitHub release, downloaded once per machine into
   `$CARGO_HOME/fdk-aac-prebuilt/`.

(3) is what makes a fresh clone of a consuming project build with nothing installed. The cache
living under `CARGO_HOME` means the many Docker builds that already cache `~/.cargo` get it
for free. To confirm which one was used:

```sh
cargo build -vv 2>&1 | grep 'cargo:info=fdk-aac'
# cargo:info=fdk-aac 2.0.3 linked statically from prebuilt/linux-x86_64 (x86_64-unknown-linux-gnu)
# cargo:info=fdk-aac cpu_floor x86-64-v3 / Coffee Lake (AVX2+FMA permitted)
# cargo:info=fdk-aac cxx_runtime none
```

Two integrity checks run on the way in, on every resolution path: the downloaded `.tar.gz`
against the `SHA256SUMS` published beside it, and the extracted `.a`/`.lib` against the
`sha256(library)` line in its own MANIFEST. Both are **corruption** checks, not tamper checks
— each list travels with the files it covers. They earn their place because corruption is what
actually happens, and each case otherwise surfaces as a page of undefined symbols rather than
one sentence naming the file. The checksum that constrains somebody *other than us* is in
`fdk-aac.env`: the SHA-256 of Fraunhofer's source tarball, enforced before a compiler runs.

### CPU floors

The x86_64 archives are built to **x86-64-v3 — AVX2, Coffee Lake or newer**, matching
`libopus-prebuilt` so that a project linking both gains no exclusion it did not already have.
Stated plainly, the cost is that they may execute an illegal instruction on anything without
AVX2: pre-2013 Intel, pre-Zen AMD, and the Celeron and Pentium parts *of* the Coffee Lake
generation, where AVX2 is fused off.

What the floor buys here is less than it buys for Opus, and the difference is worth stating
rather than glossing. opus ships hand-written SSE4.1 and AVX2 kernels, and the flag decides
whether they are *called*. fdk-aac ships no x86 SIMD at all — it is fixed-point integer code,
and its hand-written assembly is 32-bit ARM only, guarded on `__arm__` and `__ARM_ARCH_8__`,
so none of it applies on aarch64 either. All the floor can do is let the compiler
autovectorise the scalar loops. `build.sh` records how many AVX instructions ended up in the
archive as MANIFEST evidence rather than asserting a threshold, because with no hand-written
kernels there is no number anything guarantees.

A project that needs to run below the floor should build its own and point
`FDK_AAC_PREBUILT_DIR` at the prefix.

Two things this does *not* do, both on purpose: `-march=native` (the artifact runs on machines
other than the builder) and any fast-math flag (it would change the codec's arithmetic, and
the cross-target comparison below would stop meaning anything).

### Reproducibility

Two runners building the same commit produce a byte-identical `libfdk-aac.a`, and CI asserts
it. That takes one deliberate step: fdk-aac compiles `__DATE__` and `__TIME__` into eight of
its modules — `aacEncGetLibInfo` reports them as `build_date`/`build_time` — and `__TIME__` is
evaluated per translation unit, so without intervention a single build embeds a spread of
timestamps across however many seconds the compile took. `build.sh` exports
`SOURCE_DATE_EPOCH`, which GCC and Clang honour, and the archives report `Jan  1 1980`.

**`windows-x86_64-msvc` is excepted**: cl.exe supports no such variable and offers no way to
redefine `__DATE__`, so that archive's hash moves between builds. The CI job builds
`linux-x86_64` only, rather than asserting something untrue about the others.

Note also that the `.tar.gz` around an archive is *not* reproducible — gzip stamps an mtime
into its header. That is why `sha256(library)` in each MANIFEST is the checksum worth
comparing between releases, and the tarball's is only good for catching a bad download.

### How far the four targets agree

Not bit-for-bit, and finding out why is the most interesting thing CI has done here.

fdk-aac is usually described — including by an earlier version of this file — as fixed-point
integer code throughout, from which it follows that every target should encode identical
bytes. The first pipeline run disproved it: `macos-arm64` and `linux-aarch64` matched each
other exactly, and `linux-x86_64` differed on all seven configurations.

It is not a miscompile, not floating point, and **not the optimization flags**. On x86,
`fixmul.h` and `fixpoint_math.h` include `x86/fixmul_x86.h` and `x86/fixpoint_math_x86.h`,
which replace `sqrtFixp`, `invSqrtNorm2`, both overloads of `invFixp` and `schur_div` with
x86-specific implementations. aarch64 uses the generic C versions, the `arm/` headers holding
only 32-bit ARM inline assembly. Different algorithms for the same function round differently,
the encoder makes marginally different quantisation decisions, and the bitstream differs.

The full matrix says this cleanly, and it is worth reading carefully before anyone proposes
weakening a `-march` to make the numbers line up:

```
arm64:   macos-arm64         ≡ linux-aarch64      (byte-identical)
x86_64:  windows-x86_64-msvc ≡ linux-x86_64       (byte-identical)
         arm64 ≠ x86_64
```

The boundary is the **architecture**, not the toolchain. MSVC with `/arch:AVX2` and GCC with
`-march=x86-64-v3` emit the same bitstream as each other despite sharing no optimizer; Apple
clang with `-mcpu=apple-m1` and GCC with no floor at all likewise. Meanwhile the *same* GCC on
two architectures disagrees. So the CPU floors cannot be what causes the difference, and
dropping them would cost speed while changing nothing. Rebuilding `linux-x86_64` with
`-ffp-contract=off` also changes not one digest, ruling out the other obvious suspect.

So `compare-targets.py` asserts each property at the strength it actually holds:

| property | across | how equal |
|---|---|---|
| granule, decoded rate, channels, access units, decoded length | every target | exactly |
| encoded size | every target | within 2% (measured spread: under 0.031%) |
| decoded audio | every target | SNR above a per-configuration floor, correlation ≥ 0.995 |
| bitstream SHA-256 | targets of the same architecture | byte-identical |

The audio floors are per-configuration for the same reason `common::Signal` carries a
`corr_floor` per signal class: measured x86_64-against-aarch64 agreement runs from 29 dB to
79 dB depending on whether SBR is running, and one global number would either pass a broken
AAC-LC path or fail a working HE-AAC one. HE-AAC is the low one because SBR synthesises the
top octave from parameters rather than coding its waveform — at 29.4 dB it still correlates at
0.999423 with an RMS ratio of 1.00067, which is the same audio and not the same samples.

The digest comparison is scoped to one architecture because that is where it holds, and there
it holds strongly — across compilers, across CPU floors, and across libraries that are not
themselves identical. On x86_64, MSVC's archive and GCC's are byte-identical in what they
encode while sharing no optimizer; on arm64, Apple clang's and GCC's are, one built to
`-mcpu=apple-m1` and the other to no floor at all. The four runners are four different real
machines, which is why this is the evidence worth quoting: the archives differ, the bitstreams
do not.

Live runners rather than a checked-in expected value throughout: a stored digest could only
record the answer from whichever machine last regenerated it, which is the thing under test.

## Building and testing it

```sh
./build.sh linux-x86_64          # one static archive → dist/<target>/
./sync-prebuilt.sh               # dist/ → the crate's prebuilt/ cache
cargo test --offline --workspace
cargo build --offline --release --workspace
./target/release/fdk-aac-e2e
./check-static.sh target/release/fdk-aac-e2e
./test-docker.sh                 # both Linux targets, plus valgrind and its control
```

`./sync-prebuilt.sh --check` verifies the whole generated chain: that the committed headers
are byte-identical to the pinned tarball's, that `src/bindings.rs` is what bindgen makes of
those headers, and that `src/version.rs` is what the pinned sources say. CI runs it.

The generated files are committed rather than produced at build time, because a `-sys` crate
whose selling point is needing no toolchain cannot then require an LLVM installation. bindgen
is pinned to 0.72.1 — its output is not stable across its own releases, and an unpinned
generator turns `--check` into a test of which bindgen the runner installed.

### Which library got linked

fdk-aac reports no package version at runtime — there is no `opus_get_version_string()`
equivalent, and the 2.0.3 in its CMakeLists never reaches a compiler. So `gen-version.sh`
lifts the encoder and decoder *module* versions out of the pinned sources into
`src/version.rs`, and the tests require the linked archive to report exactly those:

```rust
assert_eq!(module_version, fdk_aac::version::packed(fdk_aac::version::ENCODER_LIB_VERSION));
```

That is what catches a stale cache, a system libfdk-aac winning the link, or a release that
shipped the wrong archive.

## Releasing

`.github/workflows/release.yml`, run by hand. It calls `build.yml` rather than repeating it,
so the archives that get published are the ones that passed the same tests. Draft first,
publish last — **a draft release does not create the git tag**, so four builds, their tests,
the e2e binaries, the cross-target digest comparison, packaging and upload all happen while
the tag still does not exist. A failed release leaves a deletable draft rather than a tag
pointing at archives nobody should link.

The tag is computed, never typed: `v<version>-<YYYYMMDDHHMMSS>-<short sha>`.

**Bootstrap order for a fresh repository:** two CI jobs (`leaks` and `consumer-fetch`) resolve
their archive by downloading it, so they cannot pass before the first release exists. Run
`build.yml` by hand to check the four targets compile, then `release.yml`, and CI is green
from that commit onwards.

## Something to listen to

[`wav-demo/`](wav-demo/) is a standalone project that encodes a WAV to playable `.aac` files
at several bitrates — plus the decode and the *difference* between them, so you can hear what
the codec discarded. `--profiles` compares AAC-LC against HE-AAC and HE-AAC v2 at 32 kbps and
shows, measured by ffmpeg's own decoder, the 32 dB of top octave that SBR puts back and that
upstream 0.8.0 threw away. Nothing in CI builds it.

## Licensing

**Read this before shipping anything built with it.**

The codec is under the **Fraunhofer FDK AAC Codec Library licence** — [`LICENSE`](LICENSE),
copied verbatim from the pinned tarball. It is **not OSI-approved**, and it explicitly grants
**no patent rights**: its own text says patent licences for AAC "may be obtained through Via
Licensing or through the respective patent owners individually". That obligation is real, it
travels with these archives, and it is the consumer's to satisfy — this repository cannot and
does not grant anything about it.

The Rust binding in `crates/fdk-aac-prebuilt` is a different thing from the codec it binds: it
descends from `fdk-aac` 0.8.0 by Hailey Somerville and is MIT
([`LICENSE-MIT`](crates/fdk-aac-prebuilt/LICENSE-MIT)).

Nothing here is published to crates.io. The archives are large binaries, and the licence would
need arguing with anyway.
