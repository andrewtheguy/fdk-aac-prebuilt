# wav-demo

A WAV goes in, AAC happens to it, a WAV comes out — so you can listen to the difference.

```sh
cargo run --release                                  # synthesises a 10 s clip first
cargo run --release -- --music                       # a four-bar progression instead
cargo run --release -- some.wav
cargo run --release -- some.wav --bitrates 32,64,128
cargo run --release -- some.wav --profiles           # AAC-LC vs HE-AAC vs HE-AAC v2
```

## `--music` — the clip to actually listen to

The default synthesised clip is a **drone**: its bass and chord never stop and never
re-articulate, so in ten seconds the only transients are the hi-hats. That makes it a fair
stress test and a poor listening test, because steady tones are the easiest thing AAC codes
and their artefacts are the least like the ones anybody hears in practice.

`--music` synthesises a four-bar Am–F–C–G progression at 96 bpm instead — plucked notes with
real attacks and decays, a bass line on beats 1 and 3, and hi-hats on every eighth. Ported
from the sibling [`libopus-prebuilt`](https://github.com/andrewtheguy/libopus-prebuilt) demo,
which had the better clip.

```
    kbps    frames    bytes    ratio    actual   SNR dB   delay (reported/measured)
      32       471    40989    46.8x     32.8k     18.0     3792 / 3792
      64       471    80858    23.7x     64.7k     20.9     3792 / 3792
     128       471   160790    11.9x    128.6k     22.2     3792 / 3792
```

**The SNR column is lower than the drone's and that is the point, not a regression.** 22 dB
at 128 kbps against the drone's 34 dB is a waveform measure being handed transients and noise,
which is exactly the content it scores worst and the ear notices least. Listen to
`music-128k-diff.wav`: the error is concentrated in the hi-hats and the note attacks.

It is also the better `--profiles` input, because the hi-hats give it a top octave that the
drone barely has:

| file | mean level above 13 kHz |
|---|---|
| `music-original.wav` | −39.6 dB |
| `music-aaclc.aac` (32 kbps) | **−84.3 dB** — gone |
| `music-heaac.aac` (32 kbps) | **−49.6 dB** — SBR put it back |
| `music-heaacv2.aac` (32 kbps) | −46.9 dB |

Nearly 35 dB between the first two, at one bitrate, from an independent decoder.

```
fdk-aac:  2.0.3 (encoder lib (4, 0, 1))
input:    44100 Hz, stereo, 10.00 s, 441000 frames

    kbps    frames    bytes    ratio    actual   SNR dB   delay (reported/measured)
      32       433    40235    43.8x     32.2k     28.1     3733 / 3733
      64       433    80450    21.9x     64.4k     32.5     3733 / 3733
     128       433   160868    11.0x    128.7k     34.5     3733 / 3733
```

For each setting it writes into `out/`:

| file | what it is |
|---|---|
| `<name>-<setting>.aac` | **the real thing** — ADTS AAC, playable in any player |
| `<name>-original.wav` | the input, so the A/B is against a file and not a memory |
| `<name>-<setting>.wav` | the same stream decoded back, time-aligned with the original |
| `<name>-<setting>-diff.wav` | what AAC discarded, gained up so it is audible |

```sh
ffplay -autoexit out/demo-64k.aac      # or VLC, or drag it into a browser
ffplay -autoexit out/demo-original.wav
ffplay -autoexit out/demo-64k.wav
ffplay -autoexit out/demo-64k-diff.wav # the error signal on its own
```

The difference file is the interesting one. At 128 kbps it is 34 dB down and sounds like faint
hiss around the transients; at 32 kbps you can hear the hi-hats in it, which is the codec
telling you where it stopped spending bits.

## The .aac files need no container

This is the one place AAC is *easier* than Opus. `encode` returns ADTS frames, and every one
carries a header naming the profile, the sample rate and the channel count — so the bytes go
to disk unchanged and players open them. The sibling `libopus-prebuilt` demo has to
hand-write about 150 lines of Ogg framing, page lacing and CRC before anything will play its
output, because bare Opus packets describe nothing about themselves.

Verified against a tool that had no part in writing them:

```
$ ffprobe -v error -show_entries stream=codec_name,profile,sample_rate,channels \
          -show_entries format=duration,bit_rate -of default=noprint_wrappers=1 out/demo-64k.aac
codec_name=aac
profile=LC
sample_rate=44100
channels=2
duration=10.057036
bit_rate=63994
```

10.057 s for a 10.000 s input — the extra 57 ms is the encoder delay, still in the file
because a bare ADTS stream has nowhere to record that its first frames should be skipped.
(That is what `nDelay` is for, and what an MP4 `edts` box would carry.)

## `--profiles` — where HE-AAC earns its place

```
all three at 32 kbps — low enough that the extensions matter

    profile         granule    frames    bytes   SNR dB   delay (reported/measured)
     AAC-LC          1024       433    40235     28.1     3733 / 3733
     HE-AAC          2048       219    40682     26.7     8729 / 7766
  HE-AAC v2          2048       220    40868     24.5    10777 / 9815
```

**The granule column is the proof.** 2048 rather than 1024 means dual-rate SBR is running,
with the AAC core at half the input rate and the top octave synthesised. Against upstream
`fdk-aac` 0.8.0 all three rows read 1024 — it set `AACENC_SBR_MODE` to 0 on every encoder it
created, so both HE profiles silently produced plain AAC-LC. This demo could not have been
written against it.

And the same thing measured from outside, by ffmpeg's decoder, as energy above 13 kHz:

| file | mean level above 13 kHz |
|---|---|
| `demo-original.wav` | −49.3 dB |
| `demo-aaclc.aac` (32 kbps) | **−90.3 dB** — the band is simply gone |
| `demo-heaac.aac` (32 kbps) | **−58.3 dB** — SBR put it back |
| `demo-heaacv2.aac` (32 kbps) | −55.9 dB |

```sh
ffmpeg -i out/demo-heaac.aac -af "highpass=f=13000:poles=2,highpass=f=13000:poles=2,volumedetect" -f null -
```

Thirty-two decibels between the first two rows, at the same bitrate, from an independent
decoder. That is what SBR does and what upstream disabled.

## Notes on what it does

**Time alignment, and why it is measured.** AAC has an encoder delay and the decoder adds its
own; `InfoStruct::nDelay` and `CStreamInfo::outputDelay` report them. For AAC-LC their sum is
exactly right — the two delay columns above agree to the sample. For the HE profiles it is
not: SBR adds a delay the encoder's figure does not include, and the reported sum is about
960 samples too large. So the demo finds the alignment by cross-correlation and prints both
numbers, rather than trusting the reported one and blaming the codec.

That is not a hypothetical. Aligning the HE profiles by the reported sum makes the error
signal come out *louder than the audio* — a negative SNR — which looks exactly like a broken
encoder and is a broken measurement.

**SNR is not quality**, and it is least meaningful in the `--profiles` table. It is a
waveform comparison, and SBR does not reproduce the waveform of the top octave — it
synthesises something with the right spectral shape. The HE rows are being scored on a
criterion they are not trying to meet. Trust your ears over the column.

**The afterburner is on.** fdk-aac leaves it off by default and its own documentation
recommends enabling it for anything that is not real-time. A file conversion is not.

**16-bit PCM only**, mono or stereo. The WAV reader walks the RIFF chunk list rather than
assuming a 44-byte header, so files with a `LIST`/`INFO` chunk before `data` work.

**Sample rates.** AAC accepts 8 to 96 kHz from a fixed table and there is no resampler here,
so a rate outside it is rejected with the conversion command to run.

## The dependency line

`Cargo.toml` takes the crate by **path**, so the demo builds in a fresh clone that predates
the first release. A real consumer writes the git form instead:

```toml
fdk-aac = { package = "fdk-aac-prebuilt", git = "https://github.com/andrewtheguy/fdk-aac-prebuilt", tag = "v2.0.3-…" }
```

If you switch this directory to that form, **do not bump the tag on every release.** Nothing
in CI builds this directory, an older tag still resolves, and `build.rs` fetches archives
from the repository's *latest* release regardless of which tag the crate source came from —
so a stale pin here demonstrates exactly what a current one would. Bumping it every time
would mean a commit whose only content is a tag, which is the maintenance that
`releases/latest/download/…` exists to avoid everywhere else.
