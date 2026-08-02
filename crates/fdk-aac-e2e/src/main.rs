//! End-to-end check: a real binary, doing real AAC work, run on the machine that built the
//! archive it links.
//!
//! This is not a duplicate of the test suite. The tests prove the API is wired up correctly;
//! a *binary* proves the things a test harness cannot:
//!
//! - the static archive links into a shippable executable, in release mode, with no
//!   libfdk-aac anywhere on the system and nothing to install (`check-static.sh` asserts the
//!   linkage is static and that no C++ runtime crept in, rather than assuming either);
//! - the CPU floor is real — this runs on the runner's own processor, so an archive built
//!   for AVX2 that cannot execute is a failed pipeline rather than a support ticket;
//! - Windows works at all, which is the one target that cannot be built or run anywhere
//!   else in this repository.
//!
//! # What this writes, and what compares it
//!
//! Three artifacts, because the four targets agree at three different strengths and running
//! them together under one assertion gets the answer wrong.
//!
//! `digests.txt` — a SHA-256 of each bitstream. Equal across targets **of the same
//! architecture**, and that is where the comparison is scoped. It holds strongly: the same
//! seven digests come out of a Docker container on an Apple-silicon Mac and out of a GitHub
//! `ubuntu-24.04-arm` runner, built by two different GCC versions producing two different
//! libraries (274 versus 282 NEON instructions, different `sha256(library)`). Same
//! architecture, same bytes, regardless of toolchain.
//!
//! `e2e-out/structure.txt` — granule, decoded sample rate, decoded channel count, access-unit
//! count and decoded length. These are exact across *every* target, architecture included:
//! they follow from the format and the input, not from arithmetic.
//!
//! `e2e-out/audio/<label>-source.wav` and `-decoded.wav` — what went into the encoder and
//! what came out of the decoder, for the pipeline to measure cross-architecture agreement as
//! an SNR, and for a person to listen to.
//!
//! # Why not one digest comparison across all four
//!
//! Because fdk-aac is not the uniform fixed-point integer code it is usually described as,
//! and this repository asserted that it was until CI disproved it. On x86, `fixmul.h` and
//! `fixpoint_math.h` include `x86/fixmul_x86.h` and `x86/fixpoint_math_x86.h`, which replace
//! `sqrtFixp`, `invSqrtNorm2`, both overloads of `invFixp` and `schur_div` with x86-specific
//! implementations. aarch64 uses the generic C ones — the `arm/` headers hold 32-bit ARM
//! inline assembly that is inactive on aarch64. Different algorithms for the same function
//! round differently, the encoder makes slightly different quantisation decisions, and the
//! bitstream differs. By design, in upstream, and not a miscompile.
//!
//! Neither floating point nor the CPU floors do this, which is worth recording because both
//! are the obvious suspects and both are innocent. `-ffp-contract=off` on the x86-64-v3 build
//! changes not one digest. And the four targets split strictly by architecture rather than by
//! toolchain: MSVC at `/arch:AVX2` matches GCC at `-march=x86-64-v3` byte for byte, Apple
//! clang at `-mcpu=apple-m1` matches GCC at no floor at all, and it is the same GCC across
//! two architectures that disagrees. Lowering a floor would cost speed and fix nothing.
//!
//! Nothing is compared against a *stored* value. A checked-in expected digest would have to be
//! regenerated every time the pinned fdk-aac moves, and only ever from whichever machine
//! happened to run the script — so it would encode "the answer from that machine", which is
//! the thing under test. Comparing live runners to each other has no such blind spot.
//!
//! Every check reports, and the exit code is the verdict. Failures do not stop the run,
//! because in CI the whole report is more useful than the first line of it.

mod metrics;
mod sha256;
mod signals;
mod wav;

use fdk_aac::dec::{Decoder, Transport as DecTransport};
use fdk_aac::enc::{
    AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Param, Transport,
};
use fdk_aac::version;
use fdk_aac_sys as sys;
use std::process::ExitCode;

struct Report {
    passed: usize,
    failed: Vec<String>,
    /// `label sha256` per configuration, written to digests.txt for the pipeline to compare
    /// between targets of the same architecture.
    digests: Vec<(String, String)>,
    /// One line per configuration of facts that follow from the format rather than from
    /// arithmetic, so every target must produce this file identically — architecture
    /// included. Written to e2e-out/structure.txt for the pipeline to diff.
    structure: Vec<String>,
}

impl Report {
    /// One named claim, one line of output. `ok` is the claim being true.
    fn check(&mut self, what: &str, ok: bool) {
        if ok {
            self.passed += 1;
            println!("  ok    {what}");
        } else {
            self.failed.push(what.to_string());
            println!("  FAIL  {what}");
        }
    }
}

/// One encoder configuration, encoded from a deterministic signal.
struct Config {
    label: &'static str,
    rate: u32,
    channels: ChannelMode,
    aot: AudioObjectType,
    transport: Transport,
    bitrate: BitRate,
}

/// The battery. Chosen to move fdk-aac between its distinct code paths rather than to be
/// exhaustive: AAC-LC against HE-AAC and HE-AAC v2 exercises the SBR and PS encoders,
/// several rates exercise the filterbank's different transform sizes, mono against stereo
/// exercises the joint-stereo decisions, and ADTS against LOAS and raw exercises three
/// separate transport writers.
const CONFIGS: &[Config] = &[
    Config {
        label: "lc-16k-mono-32k-adts",
        rate: 16_000,
        channels: ChannelMode::Mono,
        aot: AudioObjectType::Mpeg4LowComplexity,
        transport: Transport::Adts,
        bitrate: BitRate::Cbr(32_000),
    },
    Config {
        label: "lc-44k1-stereo-128k-adts",
        rate: 44_100,
        channels: ChannelMode::Stereo,
        aot: AudioObjectType::Mpeg4LowComplexity,
        transport: Transport::Adts,
        bitrate: BitRate::Cbr(128_000),
    },
    Config {
        label: "lc-48k-stereo-vbr3-adts",
        rate: 48_000,
        channels: ChannelMode::Stereo,
        aot: AudioObjectType::Mpeg4LowComplexity,
        transport: Transport::Adts,
        bitrate: BitRate::VbrMedium,
    },
    Config {
        label: "lc-48k-mono-64k-raw",
        rate: 48_000,
        channels: ChannelMode::Mono,
        aot: AudioObjectType::Mpeg4LowComplexity,
        transport: Transport::Raw,
        bitrate: BitRate::Cbr(64_000),
    },
    Config {
        label: "he-32k-stereo-48k-adts",
        rate: 32_000,
        channels: ChannelMode::Stereo,
        aot: AudioObjectType::Mpeg4HeAac,
        transport: Transport::Adts,
        bitrate: BitRate::Cbr(48_000),
    },
    Config {
        label: "hev2-44k1-stereo-32k-loas",
        rate: 44_100,
        channels: ChannelMode::Stereo,
        aot: AudioObjectType::Mpeg4HeAacV2,
        transport: Transport::Loas,
        bitrate: BitRate::Cbr(32_000),
    },
    Config {
        label: "eld-32k-mono-64k-raw",
        rate: 32_000,
        channels: ChannelMode::Mono,
        aot: AudioObjectType::Mpeg4EnhancedLowDelay,
        transport: Transport::Raw,
        bitrate: BitRate::Cbr(64_000),
    },
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let leak_only = args.iter().any(|a| a == "--leak-only");

    println!("fdk-aac-prebuilt end-to-end");
    println!("  fdk-aac      {}", version::FDK_AAC_VERSION);
    println!("  encoder lib  {:?}", version::ENCODER_LIB_VERSION);
    println!("  decoder lib  {:?}", version::DECODER_LIB_VERSION);
    println!("  host         {}-{}", std::env::consts::ARCH, std::env::consts::OS);

    if leak_only {
        // The control for the leak checkers. `leaks` on macOS and valgrind on Linux are only
        // evidence if they can see this library's allocations at all — fdk-aac allocates
        // with C `malloc` through libSYS, which a Rust GlobalAlloc counter cannot observe —
        // so the pipeline runs the binary this way as well and *requires* the tool to
        // complain. A clean report from a blind instrument is worse than no report.
        return leak_deliberately();
    }

    let mut report =
        Report { passed: 0, failed: Vec::new(), digests: Vec::new(), structure: Vec::new() };

    report.check("sha256 agrees with the published vectors", sha256::self_test());
    check_library_identity(&mut report);

    for config in CONFIGS {
        run_config(&mut report, config);
    }

    if std::env::var("FDK_AAC_E2E_MEMORY").as_deref() == Ok("off") {
        println!("\nmemory churn: skipped (FDK_AAC_E2E_MEMORY=off)");
    } else {
        check_memory_churn(&mut report);
    }

    write_digests(&report);
    write_structure(&report);

    println!("\n{} checks passed", report.passed);
    if report.failed.is_empty() {
        println!("everything passed");
        ExitCode::SUCCESS
    } else {
        println!("{} FAILED:", report.failed.len());
        for failure in &report.failed {
            println!("  - {failure}");
        }
        ExitCode::FAILURE
    }
}

/// The archive must be the fdk-aac these crates were generated against.
///
/// The same claim `tests/prebuilt.rs` makes, repeated here because this binary is the thing
/// that runs on all four targets and, in the `consumer-fetch` job, against the *released*
/// archive rather than whatever `prebuilt/` happens to hold locally.
fn check_library_identity(report: &mut Report) {
    println!("\nlibrary identity");

    let mut enc_info: Vec<sys::LIB_INFO> = vec![unsafe { std::mem::zeroed() }; 32];
    let err = unsafe { sys::aacEncGetLibInfo(enc_info.as_mut_ptr()) };
    report.check("aacEncGetLibInfo succeeds", err == sys::AACENC_ERROR_AACENC_OK);
    let encoder = enc_info
        .iter()
        .find(|e| e.module_id == sys::FDK_MODULE_ID_FDK_AACENC)
        .map(|e| e.version as u32);
    report.check(
        &format!(
            "the linked encoder is {:?}, reported {:?}",
            version::ENCODER_LIB_VERSION,
            encoder.map(unpack),
        ),
        encoder == Some(version::packed(version::ENCODER_LIB_VERSION)),
    );

    let mut dec_info: Vec<sys::LIB_INFO> = vec![unsafe { std::mem::zeroed() }; 32];
    unsafe { sys::aacDecoder_GetLibInfo(dec_info.as_mut_ptr()) };
    let decoder = dec_info
        .iter()
        .find(|e| e.module_id == sys::FDK_MODULE_ID_FDK_AACDEC)
        .map(|e| e.version as u32);
    report.check(
        &format!(
            "the linked decoder is {:?}, reported {:?}",
            version::DECODER_LIB_VERSION,
            decoder.map(unpack),
        ),
        decoder == Some(version::packed(version::DECODER_LIB_VERSION)),
    );
}

/// The inverse of `version::packed`, for the log line.
fn unpack(v: u32) -> (u32, u32, u32) {
    ((v >> 24) & 0xff, (v >> 16) & 0xff, (v >> 8) & 0xff)
}

/// Encode one configuration, decode it back, measure it, and digest the bitstream.
fn run_config(report: &mut Report, config: &Config) {
    let channels = config.channels.count();
    println!(
        "\n{} — {} Hz {} {:?} {:?}",
        config.label,
        config.rate,
        if channels == 2 { "stereo" } else { "mono" },
        config.aot,
        config.transport,
    );

    let params = EncoderParams {
        bit_rate: config.bitrate,
        sample_rate: config.rate,
        transport: config.transport,
        channels: config.channels,
        audio_object_type: config.aot,
    };

    let mut encoder = match Encoder::new(params) {
        Ok(e) => e,
        Err(e) => return report.check(&format!("{}: create encoder ({e})", config.label), false),
    };

    // The afterburner on, deterministically: it changes the bitstream, so having it on makes
    // the digests cover more of the encoder than they otherwise would.
    let configured = encoder.set_param(Param::Afterburner, 1).is_ok();
    report.check(&format!("{}: encoder configures", config.label), configured);

    let info = match encoder.info() {
        Ok(i) => i,
        Err(e) => return report.check(&format!("{}: encoder info ({e})", config.label), false),
    };
    let frame = info.frameLength as usize;
    report.check(
        &format!("{}: reports a frame length and a delay", config.label),
        frame > 0 && info.nDelay > 0 && info.maxOutBufBytes > 0,
    );
    report.check(
        &format!(
            "{}: granule is {frame} samples, matching has_sbr() = {}",
            config.label,
            config.aot.has_sbr(),
        ),
        // Only the plain object types have a fixed relationship between SBR and a 2048
        // granule; ELD uses its own frame lengths, so it is exempted rather than asserted
        // wrongly.
        matches!(config.aot, AudioObjectType::Mpeg4EnhancedLowDelay)
            || (frame == 2048) == config.aot.has_sbr(),
    );

    // A deterministic signal built from integer arithmetic only — see signals.rs. Nothing
    // here may go through `f64::sin`, because two platforms' libm differ in the last bit and
    // that alone would break the cross-target digest comparison for a reason having nothing
    // to do with fdk-aac.
    let source = signals::deterministic_mono(config.rate, config.rate as usize * 2);
    let pcm = signals::interleave(config.rate, &source, channels);

    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();
    let mut access_units: Vec<usize> = Vec::new();
    for chunk in pcm.chunks(frame * channels) {
        if chunk.len() < frame * channels {
            break;
        }
        match encoder.encode(chunk, &mut out) {
            Ok(r) if r.output_size > 0 => {
                stream.extend_from_slice(&out[..r.output_size]);
                access_units.push(r.output_size);
            }
            Ok(_) => {}
            Err(e) => return report.check(&format!("{}: encode ({e})", config.label), false),
        }
    }
    while let Ok(Some(r)) = encoder.flush(&mut out) {
        if r.output_size == 0 {
            break;
        }
        stream.extend_from_slice(&out[..r.output_size]);
        access_units.push(r.output_size);
    }

    report.check(
        &format!(
            "{}: produced {} bytes in {} access units",
            config.label,
            stream.len(),
            access_units.len(),
        ),
        stream.len() > 1000 && access_units.len() > 10,
    );

    let digest = sha256::hex(&stream);
    println!("        sha256 {digest}  ({} bytes)", stream.len());
    report.digests.push((config.label.to_string(), digest));

    // Decode it back and require it to resemble what went in. The digest says every target
    // agrees; this says what they agree on is audio.
    let decode_as = match config.transport {
        Transport::Adts => DecTransport::Adts,
        Transport::Loas => DecTransport::Loas,
        Transport::Latm => DecTransport::Latm,
        Transport::Adif => DecTransport::Adif,
        Transport::Raw => DecTransport::Raw,
    };
    let mut decoder = match Decoder::new(decode_as) {
        Ok(d) => d,
        Err(e) => return report.check(&format!("{}: create decoder ({e})", config.label), false),
    };
    if config.transport == Transport::Raw {
        let ok = decoder.config_raw(&info.confBuf[..info.confSize as usize]).is_ok();
        report.check(&format!("{}: raw config accepted", config.label), ok);
    }

    let mut decoded: Vec<i16> = Vec::new();
    let mut pcm_out = vec![0i16; 16384];
    if config.transport == Transport::Raw {
        // Raw carries no framing, so the access units have to be fed one at a time — the
        // caller owns the boundaries. Concatenating them would be a decoder error rather
        // than a shortcut.
        let mut offset = 0;
        for &size in &access_units {
            if decoder.fill(&stream[offset..offset + size]).is_err() {
                break;
            }
            offset += size;
            if decoder.decode_frame(&mut pcm_out).is_ok() {
                let n = decoder.decoded_frame_size();
                decoded.extend_from_slice(&pcm_out[..n]);
            }
        }
    } else {
        let mut offset = 0;
        while offset < stream.len() {
            let taken = match decoder.fill(&stream[offset..]) {
                Ok(n) => n,
                Err(_) => break,
            };
            offset += taken;
            while decoder.decode_frame(&mut pcm_out).is_ok() {
                let n = decoder.decoded_frame_size();
                if n == 0 {
                    break;
                }
                decoded.extend_from_slice(&pcm_out[..n]);
            }
            if taken == 0 {
                break;
            }
        }
    }

    report.check(
        &format!("{}: decoded {} samples", config.label, decoded.len()),
        decoded.len() > frame * 4,
    );
    if decoded.is_empty() {
        return;
    }

    let si = decoder.stream_info();
    report.check(
        &format!("{}: decodes at {} Hz", config.label, si.sampleRate),
        si.sampleRate == config.rate as i32,
    );
    // With Parametric Stereo the core is mono and the decoder synthesises the second
    // channel, so the output channel count is what a consumer sizes buffers from.
    report.check(
        &format!("{}: decodes to {} channels", config.label, si.numChannels),
        si.numChannels as usize == channels,
    );

    // The first channel against the source, aligned by measurement.
    let left: Vec<f64> = decoded.iter().step_by(channels).map(|&s| s as f64).collect();
    let reference: Vec<f64> = source.iter().map(|&s| s as f64).collect();
    let (lag, corr) = metrics::best_lag(&reference, &left, 12_000);
    println!("        correlation {corr:.4} at lag {lag}");
    // 0.5 rather than something tighter: this battery includes HE-AAC v2 at 32 kbps, where
    // the codec synthesises the high band rather than preserving it and a waveform
    // comparison is not entitled to more. The floor is here to catch silence and garbage;
    // the per-signal thresholds that mean something live in the test suite.
    report.check(&format!("{}: round trip correlates ({corr:.4})", config.label), corr > 0.5);

    let n = left.len().saturating_sub(lag).min(reference.len());
    let ratio = metrics::rms(&left[lag..lag + n]) / metrics::rms(&reference[..n]);
    report
        .check(&format!("{}: energy ratio {ratio:.3}", config.label), (0.3..3.0).contains(&ratio));

    // The facts every target must agree on exactly. Deliberately not the bitstream length:
    // that is arithmetic, it moves with the quantisation decisions the x86 primitives make
    // differently, and putting it here would fail a comparison that is meant to be exact.
    // The pipeline compares sizes separately, with a tolerance.
    report.structure.push(format!(
        "{} granule={frame} rate={} channels={} access_units={} decoded_samples={} bytes={}",
        config.label,
        si.sampleRate,
        si.numChannels,
        access_units.len(),
        decoded.len(),
        stream.len(),
    ));
    // Both halves, so the pair can be listened to as well as measured: `-source` is exactly
    // what went into the encoder, `-decoded` exactly what came out of the decoder. The
    // pipeline's SNR job reads the second; the first is what makes the second interpretable.
    let dir = std::path::Path::new("e2e-out").join("audio");
    write_audio(&dir.join(format!("{}-source.wav", config.label)), config.rate, channels, &pcm);
    write_audio(
        &dir.join(format!("{}-decoded.wav", config.label)),
        si.sampleRate as u32,
        si.numChannels as usize,
        &decoded,
    );
}

/// Write one WAV, reporting rather than failing if it cannot.
///
/// Not a check: a read-only working directory should not fail the binary's real job, and the
/// jobs that consume these files notice a missing artifact on their own.
fn write_audio(path: &std::path::Path, rate: u32, channels: usize, samples: &[i16]) {
    if let Err(e) = wav::write(path, rate, channels as u16, samples) {
        println!("        could not write {}: {e}", path.display());
    }
}

/// Create and destroy a lot of codecs and require the process not to grow without bound.
///
/// A coarse instrument compared with `leaks` and valgrind, and it runs on all four targets
/// including Windows, where neither of those exists. It catches the failure that matters
/// most — a handle that is never closed — because that grows linearly and unmistakably.
fn check_memory_churn(report: &mut Report) {
    println!("\nmemory churn");
    let params = EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: 48_000,
        transport: Transport::Adts,
        channels: ChannelMode::Stereo,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    };

    let pcm = vec![0i16; 2048];
    let mut out = vec![0u8; 8192];
    let baseline = resident_bytes();

    for _ in 0..600 {
        if let Ok(mut encoder) = Encoder::new(params) {
            let _ = encoder.encode(&pcm, &mut out);
        }
        if let Ok(mut decoder) = Decoder::new(DecTransport::Adts) {
            let _ = decoder.fill(&out);
        }
    }

    let after = resident_bytes();
    match (baseline, after) {
        (Some(before), Some(now)) => {
            let growth = now.saturating_sub(before);
            println!("  RSS {before} -> {now} bytes ({growth} growth)");
            // 32 MiB. Six hundred leaked encoders would be far more than this — each holds
            // tens of kilobytes of state — while allocator fragmentation over the same loop
            // is far less.
            report.check(
                &format!("RSS grew by {growth} bytes over 600 codec lifetimes"),
                growth < 32 * 1024 * 1024,
            );
        }
        _ => println!("  RSS is not readable on this platform — skipped"),
    }
}

/// Leak sixty encoders and decoders on purpose, then exit successfully.
///
/// The point is the exit code: this must exit 0 while `leaks` or valgrind, watching it,
/// exits non-zero. A pipeline step that requires the *tool* to fail here is what proves the
/// clean run in the other step was a measurement rather than a blind spot.
fn leak_deliberately() -> ExitCode {
    println!("\nleaking 60 encoders and 60 decoders on purpose (--leak-only)");
    let params = EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: 48_000,
        transport: Transport::Adts,
        channels: ChannelMode::Stereo,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    };
    for _ in 0..60 {
        if let Ok(encoder) = Encoder::new(params) {
            std::mem::forget(encoder);
        }
        if let Ok(decoder) = Decoder::new(DecTransport::Adts) {
            std::mem::forget(decoder);
        }
    }
    println!("if the tool watching this process reports no leak, the tool is not working");
    ExitCode::SUCCESS
}

/// Write `digests.txt` in the working directory for the pipeline to collect.
///
/// Formatted as `sha256sum` writes it, so the bit-exact job can diff two of these directly
/// and a human can run `sha256sum -c` against a bitstream by hand.
fn write_digests(report: &Report) {
    let mut text = String::new();
    for (label, digest) in &report.digests {
        text.push_str(&format!("{digest}  {label}\n"));
    }
    match std::fs::write("digests.txt", &text) {
        Ok(()) => println!("\nwrote digests.txt ({} configurations)", report.digests.len()),
        // Not a failure: a read-only working directory should not fail the binary's real
        // job, and the bit-exact job notices a missing artifact on its own.
        Err(e) => println!("\ncould not write digests.txt: {e}"),
    }
}

/// Write `e2e-out/structure.txt` — the facts the pipeline requires every target to agree on.
///
/// A missing line is as meaningful as a wrong one: `run_config` returns early on any failure,
/// so a target that could not encode a configuration writes a shorter file, and the diff the
/// pipeline runs reports that rather than passing on the lines that did survive.
fn write_structure(report: &Report) {
    let path = std::path::Path::new("e2e-out").join("structure.txt");
    if let Err(e) = std::fs::create_dir_all("e2e-out") {
        println!("could not create e2e-out: {e}");
        return;
    }
    let text = report.structure.iter().map(|l| format!("{l}\n")).collect::<String>();
    match std::fs::write(&path, &text) {
        Ok(()) => println!("wrote {} ({} configurations)", path.display(), report.structure.len()),
        Err(e) => println!("could not write {}: {e}", path.display()),
    }
}

/// Resident set size in bytes, where the platform makes it cheap to ask.
///
/// Returns `None` rather than guessing anywhere else — a fabricated number would make the
/// churn check above report a pass it did not earn.
fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }
    #[cfg(target_os = "macos")]
    {
        // `ps` rather than task_info(), which would need a libc dependency this binary
        // deliberately does not have. Reported in kilobytes.
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().map(|kb| kb * 1024)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}
