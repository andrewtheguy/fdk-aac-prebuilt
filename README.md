# fdk-aac-prebuilt

Static **Fraunhofer FDK AAC 2.0.3**, built once with cmake so that nothing which *links* it
needs a C++ toolchain at all — plus a Rust crate that replaces `fdk-aac` + `fdk-aac-sys` and
links it.

| target | library | CPU floor |
|---|---|---|
| `macos-arm64` | `libfdk-aac.a` | `apple-m1`, deployment target 11.0 |
| `linux-x86_64` | `libfdk-aac.a` | x86-64 baseline — no floor above SSE2 |
| `linux-x86_64-v3` | `libfdk-aac.a` | **x86-64-v3 / Coffee Lake**, opt-in via the `x86-64-v3` feature |
| `linux-aarch64` | `libfdk-aac.a` | ARMv8-A baseline |
| `windows-x86_64-msvc` | `fdk-aac.lib` | x86-64 baseline, dynamic CRT |
| `windows-x86_64-msvc-v3` | `fdk-aac.lib` | **x86-64-v3 / Coffee Lake**, opt-in the same way; dynamic CRT |

Two archives per x86_64 platform, because fdk-aac has no runtime CPU dispatch of its own — no
CPUID, no SIMD kernels to select. `libopus-prebuilt` keeps opus's dispatch and one archive
runs its AVX2 kernels wherever the CPU has them; here the choice has to be made at build time,
so it is made twice and the consumer picks. The default runs on any x86-64. What the other one
buys, measured, is under **CPU floors**.

## Why

`fdk-aac-sys` 0.5 vendors the entire FDK AAC C++ tree inside the crate and compiles about
170 `.cpp` files with the `cc` crate on **every clean build** — in every CI job, every Docker
layer, every fresh clone. This repository does that compile once, in a pipeline, and
publishes the archives; a consumer's build script then finds one and emits two link flags.

Three things fall out of doing it that way, and the third was a surprise:

- no C++ compiler, no cmake, no autotools in any consuming project;
- the archives are checked — compiled to a stated CPU floor and, on x86_64, disassembled to
  prove no instruction above it got in; reproducible across runners (on the toolchains that
  can be, see below); and byte-identical in what they *encode* across targets of the same
  architecture (see **How far the targets agree** below);
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

On x86_64, add `features = ["x86-64-v3"]` to that line to link the AVX2-floored archive
instead of the baseline one. It is a feature rather than an environment variable because the
decision belongs to whoever builds the binary, in a manifest where it is reviewed; and it is
off by default because the archive it selects dies with an illegal instruction on any CPU
without AVX2. See **CPU floors** for what it is worth. On other targets it does nothing.

Three crates are involved:

| crate | what it is |
|---|---|
| `fdk-aac-prebuilt-sys` | the FFI, and a build script that finds the right archive and emits the link flags |
| `fdk-aac-prebuilt` | the safe API — `fdk-aac` 0.8.0 by Hailey Somerville, with the fixes below |
| `fdk-aac-e2e` | not a library: a consumer, written with the same dependency line you would use, built and *run* by the pipeline on every target |

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
# cargo:info=fdk-aac cpu_floor x86-64 baseline (SSE2; fdk-aac has no runtime dispatch, so nothing above it is used)
# — or, with features = ["x86-64-v3"]:
# cargo:info=fdk-aac 2.0.3 linked statically from prebuilt/linux-x86_64-v3 (x86_64-unknown-linux-gnu)
# cargo:info=fdk-aac cpu_floor x86-64-v3 / Coffee Lake (AVX2+FMA unconditional; fdk-aac has no runtime dispatch)
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

The default x86_64 archives have **no floor above the x86-64 baseline**. They were built to
x86-64-v3 — AVX2, Coffee Lake or newer, matching `libopus-prebuilt` at the time — until a
binary linking that repository's archive died on the first AVX2 instruction on an Ivy Bridge
i5-3210M. Both repositories dropped the floor as the default for the same reason: an archive
that is merely slower is a number in a MANIFEST, and one that SIGILLs is a support call from
whoever runs the oldest machine. Here the floored build survives as a second, opt-in archive
per platform, `linux-x86_64-v3` and `windows-x86_64-msvc-v3`, selected by the crate's
`x86-64-v3` feature.

What replaced the floor there cannot be replicated here, and the difference is worth stating
rather than glossing. opus compiles its SSE4.1 and AVX2 kernels per file and selects one by
CPUID at run time, so libopus-prebuilt lost nothing by dropping its `-march`: a Coffee Lake
still runs the AVX2 kernels. fdk-aac has no such mechanism to keep. There is no CPUID query,
no function multiversioning and no SIMD kernel anywhere in it; its per-architecture code is a
handful of *scalar* overrides — on x86 an inline `imul` for the fixed-point multiply and four
float-based math routines, on ARM inline assembly for the multiplies and `clz` (see the next
section, which this file used to get wrong). So the only thing a floor ever decides here is
which instructions the compiler's **autovectorizer** may use for the scalar loops, and the
only way to build one archive that runs on every x86-64 is the baseline. It still vectorizes
at the baseline: SSE2 is part of x86-64, and the linux-x86_64 MANIFEST records 10,918 SSE2
packed-integer instructions where linux-x86_64-v3's records 23,589 AVX ones.

What the `-v3` flavour buys, measured on an i5-8500T with 60 s of 48 kHz stereo, afterburner
on, best of three runs, twice:

| configuration | x86-64-v3 | baseline | output |
|---|---|---|---|
| AAC-LC 128 kbps | 0.97–1.01% of a core | 1.07% | identical bytes |
| HE-AAC 64 kbps | 1.60% | 1.66–1.70% | identical bytes |
| HE-AAC v2 32 kbps | 0.92% | 0.97–0.98% | identical bytes |

Five to ten percent, of roughly one percent of a core, and the seven e2e digests are
byte-identical between the two flavours — the pipeline's compare job asserts that on every
CI run, since both land in the same x86_64 digest group. A project that has measured its
own workload and knows every machine it ships to has AVX2 can take it with one word in its
manifest; the price is that the binary then excludes pre-2013 Intel, pre-Zen AMD, and the
Celeron and Pentium parts *of* the Coffee Lake generation, where AVX2 is fused off. Anything
else — `-march=native` for a fleet of identical machines, say — is still `FDK_AAC_PREBUILT_DIR`.

`build.sh` asserts each flavour's property rather than trusting the script that is supposed
to produce it, and the two assertions are opposites. For the baseline, the cmake cache must
carry no `-march`, `-mcpu`, `-mtune` or `/arch:` — the flags are passed explicitly even when
empty, so a `CXXFLAGS=-march=native` in the runner's environment cannot seed them — and on
Linux the disassembled archive must contain **no AVX instruction at all**, by mnemonic, since
with no dispatch one is one machine it will not run on. For `-v3`, the flags must have reached
the compiler and the disassembly must contain AVX instructions, or the floor did nothing and
the MANIFEST would be claiming a speed the archive does not have. The first run of the
baseline check caught the build directory's stale cache still holding `-march=x86-64-v3` from
the previous build, which cmake had kept and reported `[100%] Built target` on without
compiling a file; `build.sh` now starts from a clean build directory. Windows has no
disassembler on the runner and the runner's own CPU has AVX2, so the cache check is what
covers those two targets, and their MANIFESTs say so.

The `-v3` archive is also byte-identical to the one this repository published before the
floor was dropped — same flags, `-mtune=skylake` included, same `sha256(library)` — so the
reproducibility comparison across releases holds across the change.

`macos-arm64` is built for `apple-m1`, which every arm64 Mac satisfies, and `linux-aarch64` is
baseline ARMv8-A: arm64 Linux spans a decade of very different cores, NEON is mandatory in
ARMv8-A anyway, and fdk-aac has nothing above the baseline to unlock.

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

### How far the targets agree

Not bit-for-bit, and finding out why is the most interesting thing CI has done here.

fdk-aac is usually described — including by an earlier version of this file — as fixed-point
integer code throughout, from which it follows that every target should encode identical
bytes. The first pipeline run disproved it: `macos-arm64` and `linux-aarch64` matched each
other exactly, and `linux-x86_64` differed on all seven configurations.

It is not a miscompile and **not the optimization flags** — the matrix below settles that. It
is fdk-aac's per-architecture arithmetic, and an earlier version of this file, and of the
pipeline's comments, had half of that wrong. They said the `arm/` headers held only 32-bit ARM
assembly and that aarch64 ran the generic C. Neither is true: `FDK_archdef.h` defines
`__arm__` *and* `__ARM_ARCH_8__` from `__aarch64__`, and under those two macros the `arm/`
headers carry dedicated A64 inline assembly — `smull`/`asr` for `fixmuldiv2_DD` and
`fixmul_DD`, `smaddl`/`smsubl` for `cplxMultDiv2`, `clz` for `fixnormz_D` — plus
`#if defined(__arm__)` paths in nine `.cpp` files, all of them live on both arm64 targets. On x86, `fixmul_x86.h` supplies an `imul` and `fixpoint_math_x86.h` replaces
`sqrtFixp`, `invSqrtNorm2`, both overloads of `invFixp` and `schur_div` with implementations
that go through `float` — `sqrtf`, `1.0/sqrt`, `frexpf`/`ldexpf` — and truncate back to fixed
point.

Which corrects a second earlier claim, that the difference "is not floating point". Four of
the x86 routines *are* computed in floating point. What the difference is not is floating-point
**nondeterminism**: IEEE square root and division are correctly rounded, none of those four
contains an `a*b+c` for FMA contraction to change, and x86-64 has no x87 excess precision —
which is why MSVC and GCC agree to the byte, and why `-ffp-contract=off` changes nothing.

So the two architectures each replace the generic routines with their own, and the two sets
round differently from each other. `fixmul_DD` alone shows it: on aarch64 it is
`(a·b) >> 31`, on x86 and in the generic C it is `((a·b) >> 32) << 1`, one bit apart whenever
bit 31 of the product is set. Which of the overrides the divergence actually runs through was
not isolated, and does not need to be — the encoder's quantisation decisions sit close enough
to those bits that the bitstream differs, and the audio does not.

The full matrix says this cleanly. It was first measured when x86-64-v3 was the only x86_64
flavour; the baseline flavour reproduces all seven digests, which is one more data point
that the flags are innocent, and now that both flavours are built on every run the compare
job re-asserts it each time:

```
arm64:   macos-arm64         ≡ linux-aarch64      (byte-identical)
x86_64:  windows-x86_64-msvc ≡ linux-x86_64       (byte-identical)
         arm64 ≠ x86_64
```

The boundary is the **architecture**, not the toolchain. MSVC at `/arch:AVX2` and GCC at
`-march=x86-64-v3` emitted the same bitstream as each other despite sharing no optimizer, and
GCC at the baseline emits it again; Apple clang with `-mcpu=apple-m1` and GCC with no floor at
all likewise. Meanwhile the *same* GCC on two architectures disagrees. So the CPU floors cannot
be what causes the difference — they were dropped on x86_64 for a different reason, above, and
not one digest moved. Rebuilding `linux-x86_64` with `-ffp-contract=off` also changes nothing.

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
encode while sharing no optimizer, and GCC's rebuilt at the baseline encodes the same bytes
again; on arm64, Apple clang's and GCC's are, one built to `-mcpu=apple-m1` and the other to
no floor at all. The archives come from four different real machines, which is why this is
the evidence worth quoting: the archives differ, the bitstreams do not.

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
./test-docker.sh                 # every Linux target, plus valgrind and its control
```

For the `-v3` flavour, `./build.sh linux-x86_64-v3` and then `--features fdk-aac-e2e/x86-64-v3`
on each cargo command, which is how the pipeline links it too — the same way a consumer would.

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
publish last — **a draft release does not create the git tag**, so six builds, their tests,
the e2e binaries, the cross-target digest comparison, packaging and upload all happen while
the tag still does not exist. A failed release leaves a deletable draft rather than a tag
pointing at archives nobody should link.

The tag is computed, never typed: `v<version>-<YYYYMMDDHHMMSS>-<short sha>`.

**Bootstrap order for a fresh repository:** two CI jobs (`leaks` and `consumer-fetch`) resolve
their archive by downloading it, so they cannot pass before the first release exists. Run
`build.yml` by hand to check the six targets compile, then `release.yml`, and CI is green
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
