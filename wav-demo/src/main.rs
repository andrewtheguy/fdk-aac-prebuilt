//! Encode a WAV through AAC and decode it back, so you can listen to the result.
//!
//!   cargo run --release                     # synthesises a demo clip first
//!   cargo run --release -- some.wav
//!   cargo run --release -- some.wav --bitrates 32,64,128
//!   cargo run --release -- some.wav --profiles       # AAC-LC vs HE-AAC vs HE-AAC v2
//!
//! For each setting it writes a playable `.aac` file, the decoded round trip as `.wav`, and
//! the *difference* between them gained up so it is audible.
//!
//! Two things are worth knowing about what this does.
//!
//! **The `.aac` files are real files.** AAC in ADTS is self-framing and self-describing —
//! every frame carries a header naming the profile, the sample rate and the channel count —
//! so the encoder's output goes to disk unchanged and players open it. That is a real
//! difference from Opus, whose raw packets carry none of that and need an Ogg container
//! written around them before anything will play them.
//!
//! **The decoded file is time-aligned with the original.** AAC has an algorithmic delay and
//! so does the decoder — `InfoStruct::nDelay` and `CStreamInfo::outputDelay` — and without
//! compensating for both, the two files are offset by several milliseconds, which makes an
//! A/B comparison sound like a phase problem that is not there.
//!
//! The two reported figures are the right answer for AAC-LC and *not* for the HE profiles,
//! where SBR adds a delay the encoder's figure does not include. So the alignment is
//! measured by cross-correlation and both numbers are printed side by side; on a plain
//! AAC-LC file they agree exactly, and on an HE-AAC file they differ by about a thousand
//! samples. Trusting the reported sum there scores the round trip as noisier than the
//! signal, which reads as a broken codec and is a broken measurement.

mod wav;

use fdk_aac::dec::{Decoder, Transport as DecTransport};
use fdk_aac::enc::{
    AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Param, Transport,
};
use std::path::{Path, PathBuf};
use wav::Wav;

/// The sample rates AAC-LC accepts. Anything else needs resampling, which this demo does not
/// do — see the error message below.
const RATES: [u32; 12] =
    [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000];

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<PathBuf> = None;
    let mut bitrates = vec![32, 64, 128];
    let mut profiles = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bitrates" => {
                i += 1;
                let list = args.get(i).ok_or("--bitrates needs a value, e.g. 32,64")?;
                bitrates = list
                    .split(',')
                    .map(|s| s.trim().parse::<u32>().map_err(|_| format!("not a number: {s}")))
                    .collect::<Result<_, _>>()?;
            }
            "--profiles" => profiles = true,
            "-h" | "--help" => {
                println!(
                    "usage: wav-demo [input.wav] [--bitrates 32,64,128] [--profiles]\n\
                     \n\
                     --profiles  encode at one low bitrate as AAC-LC, HE-AAC and HE-AAC v2,\n\
                     \x20           which is where SBR and Parametric Stereo earn their place."
                );
                return Ok(());
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => input = Some(PathBuf::from(other)),
        }
        i += 1;
    }

    let out_dir = Path::new("out");
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create out/: {e}"))?;

    let (source, stem) = match &input {
        Some(p) => {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            (wav::read(p)?, stem)
        }
        None => {
            println!("no input given — synthesising a demo clip\n");
            (demo_clip(), "demo".to_string())
        }
    };

    if !RATES.contains(&source.rate) {
        return Err(format!(
            "{} Hz is not an AAC sample rate.\n       Convert first, e.g.\n         \
             ffmpeg -i in.wav -ar 48000 -c:a pcm_s16le out.wav",
            source.rate,
        ));
    }

    println!(
        "fdk-aac:  {} (encoder lib {:?})",
        fdk_aac::version::FDK_AAC_VERSION,
        fdk_aac::version::ENCODER_LIB_VERSION
    );
    println!(
        "input:    {} Hz, {}, {:.2} s, {} frames",
        source.rate,
        if source.channels == 2 { "stereo" } else { "mono" },
        source.seconds(),
        source.frames(),
    );
    println!();

    let original = out_dir.join(format!("{stem}-original.wav"));
    wav::write(&original, &source)?;

    if profiles {
        run_profiles(&source, &stem, out_dir)
    } else {
        run_bitrates(&source, &stem, out_dir, &bitrates)
    }
}

/// One encoder configuration, all the way through.
struct Outcome {
    aac_bytes: usize,
    frames: usize,
    snr_db: f64,
    /// `nDelay + outputDelay`, what the two libraries claim.
    reported_delay: usize,
    /// What lining the waveforms up actually needed. Printed beside the reported figure
    /// because for the HE profiles they differ, and that is worth seeing rather than hiding.
    measured_delay: usize,
}

fn run_bitrates(source: &Wav, stem: &str, out_dir: &Path, bitrates: &[u32]) -> Result<(), String> {
    println!("    kbps    frames    bytes    ratio    actual   SNR dB   delay (reported/measured)");
    for &kbps in bitrates {
        let params = EncoderParams {
            bit_rate: BitRate::Cbr(kbps * 1000),
            sample_rate: source.rate,
            transport: Transport::Adts,
            channels: if source.channels == 2 { ChannelMode::Stereo } else { ChannelMode::Mono },
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        };
        let name = format!("{stem}-{kbps}k");
        let result = encode_and_decode(source, params, out_dir, &name)?;

        let raw = source.samples.len() * 2;
        let actual = result.aac_bytes as f64 * 8.0 / source.seconds() / 1000.0;
        println!(
            "{kbps:>8}  {:>8}  {:>7}  {:>6.1}x  {:>7.1}k  {:>7.1}   {:>6} / {:<6}",
            result.frames,
            result.aac_bytes,
            raw as f64 / result.aac_bytes as f64,
            actual,
            result.snr_db,
            result.reported_delay,
            result.measured_delay,
        );
    }
    print_listening_notes(stem, out_dir);
    Ok(())
}

/// AAC-LC against HE-AAC against HE-AAC v2, at a bitrate low enough that the difference is
/// the whole point.
///
/// This is the comparison upstream `fdk-aac` 0.8.0 could not produce at all: it set
/// `AACENC_SBR_MODE` to 0 unconditionally, so both HE profiles silently encoded as plain
/// AAC-LC and all three rows of this table would have been the same codec.
fn run_profiles(source: &Wav, stem: &str, out_dir: &Path) -> Result<(), String> {
    let kbps = if source.channels == 2 { 32 } else { 24 };
    println!("all three at {kbps} kbps — low enough that the extensions matter\n");
    println!("    profile         granule    frames    bytes   SNR dB   delay (reported/measured)");

    for (aot, label) in [
        (AudioObjectType::Mpeg4LowComplexity, "AAC-LC"),
        (AudioObjectType::Mpeg4HeAac, "HE-AAC"),
        (AudioObjectType::Mpeg4HeAacV2, "HE-AAC v2"),
    ] {
        if aot == AudioObjectType::Mpeg4HeAacV2 && source.channels != 2 {
            println!("{:>11}         (stereo input only)", label);
            continue;
        }
        let params = EncoderParams {
            bit_rate: BitRate::Cbr(kbps * 1000),
            sample_rate: source.rate,
            transport: Transport::Adts,
            channels: if source.channels == 2 { ChannelMode::Stereo } else { ChannelMode::Mono },
            audio_object_type: aot,
        };
        let name = format!("{stem}-{}", label.to_lowercase().replace([' ', '-'], ""));
        let granule = Encoder::new(params).map_err(|e| e.to_string())?.info().unwrap().frameLength;
        let result = encode_and_decode(source, params, out_dir, &name)?;
        println!(
            "{label:>11}  {granule:>12}  {:>8}  {:>7}  {:>7.1}   {:>6} / {:<6}",
            result.frames,
            result.aac_bytes,
            result.snr_db,
            result.reported_delay,
            result.measured_delay,
        );
    }

    println!(
        "\nThe granule column is the tell: 2048 rather than 1024 means dual-rate SBR is\n\
         running, with the AAC core at half the input rate and the top octave synthesised.\n\
         Against upstream fdk-aac 0.8.0 all three rows read 1024, because it disabled SBR\n\
         on every encoder it made.\n\
         \n\
         Note the delay columns diverge for the HE profiles: SBR adds a delay the encoder's\n\
         nDelay does not account for, so aligning by the reported figure alone leaves the\n\
         decode ~20 ms early. This demo measures the alignment instead.\n\
         \n\
         Do not read too much into the SNR column here. It is a waveform comparison, and\n\
         SBR does not reproduce the waveform of the top octave — it synthesises something\n\
         with the right spectral shape — so the HE rows are scored on a criterion they are\n\
         not trying to meet. Listen to the files."
    );
    print_listening_notes(stem, out_dir);
    Ok(())
}

fn encode_and_decode(
    source: &Wav,
    params: EncoderParams,
    out_dir: &Path,
    name: &str,
) -> Result<Outcome, String> {
    let channels = source.channels;

    let mut encoder = Encoder::new(params).map_err(|e| format!("{name}: {e}"))?;
    // The afterburner: more encoder time for better quality at the same bitrate. fdk-aac
    // leaves it off by default and its own documentation recommends turning it on for
    // anything that is not real-time, which a file conversion is not.
    encoder.set_param(Param::Afterburner, 1).map_err(|e| format!("{name}: {e}"))?;

    let info = encoder.info().map_err(|e| format!("{name}: {e}"))?;
    let frame = info.frameLength as usize * channels;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut aac = Vec::new();
    let mut frames = 0;

    for chunk in source.samples.chunks(frame) {
        // A short final chunk is padded rather than dropped, so the tail of the file is
        // encoded instead of silently truncated.
        let mut padded;
        let input = if chunk.len() == frame {
            chunk
        } else {
            padded = chunk.to_vec();
            padded.resize(frame, 0);
            &padded
        };
        let r = encoder.encode(input, &mut out).map_err(|e| format!("{name}: {e}"))?;
        if r.output_size > 0 {
            aac.extend_from_slice(&out[..r.output_size]);
            frames += 1;
        }
    }
    // Drain the encoder's lookahead. Without this the last ~2048 samples never come out —
    // which is what upstream 0.8.0 did, having no flush at all.
    while let Some(r) = encoder.flush(&mut out).map_err(|e| format!("{name}: {e}"))? {
        if r.output_size == 0 {
            break;
        }
        aac.extend_from_slice(&out[..r.output_size]);
        frames += 1;
    }

    let aac_path = out_dir.join(format!("{name}.aac"));
    std::fs::write(&aac_path, &aac)
        .map_err(|e| format!("cannot write {}: {e}", aac_path.display()))?;

    // ---- decode it back

    let mut decoder = Decoder::new(DecTransport::Adts).map_err(|e| format!("{name}: {e}"))?;
    let mut decoded: Vec<i16> = Vec::new();
    let mut pcm = vec![0i16; 16384];
    let mut offset = 0;
    while offset < aac.len() {
        let taken = decoder.fill(&aac[offset..]).map_err(|e| format!("{name}: {e}"))?;
        offset += taken;
        while decoder.decode_frame(&mut pcm).is_ok() {
            let n = decoder.decoded_frame_size();
            if n == 0 {
                break;
            }
            decoded.extend_from_slice(&pcm[..n]);
        }
        if taken == 0 {
            break;
        }
    }

    let si = decoder.stream_info();
    let reported = (info.nDelay + si.outputDelay) as usize;

    // Aligned by *measurement*, with the reported delay used only as the starting guess.
    //
    // Adding `nDelay` and `outputDelay` is right for AAC-LC and lands within a sample. It is
    // not right for the HE profiles: SBR adds its own delay that the encoder's figure does
    // not account for, and using the reported sum there leaves the decode about 40 ms early
    // — enough that a sample-by-sample comparison reports the error as *louder than the
    // signal*, which is what a negative SNR means. Explaining that away as "SBR does not
    // preserve the waveform" would have been a plausible sentence covering a measurement
    // bug, so the alignment is measured instead and both numbers are printed.
    let measured = best_lag(source, &decoded, channels, reported);
    let delay = measured * channels;
    let aligned: Vec<i16> = decoded.iter().copied().skip(delay).collect();

    let round_trip = Wav {
        rate: source.rate,
        channels,
        samples: aligned.iter().copied().take(source.samples.len()).collect(),
    };
    wav::write(&out_dir.join(format!("{name}.wav")), &round_trip)?;

    // ---- what the codec discarded

    let n = round_trip.samples.len().min(source.samples.len());
    let difference: Vec<i32> =
        (0..n).map(|i| source.samples[i] as i32 - round_trip.samples[i] as i32).collect();

    // Gained up until its peak is near full scale, so it is audible at all — at a good
    // bitrate the error is 40 dB down and inaudible without this. The gain is reported so
    // the number itself says how much was discarded.
    let peak = difference.iter().map(|d| d.unsigned_abs()).max().unwrap_or(1).max(1);
    let gain = (24_000 / peak).max(1);
    let diff_wav = Wav {
        rate: source.rate,
        channels,
        samples: difference
            .iter()
            .map(|&d| (d * gain as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect(),
    };
    wav::write(&out_dir.join(format!("{name}-diff.wav")), &diff_wav)?;

    // SNR over the aligned overlap.
    let signal: f64 = (0..n).map(|i| (source.samples[i] as f64).powi(2)).sum();
    let noise: f64 = difference.iter().map(|&d| (d as f64).powi(2)).sum();
    let snr_db = if noise > 0.0 { 10.0 * (signal / noise).log10() } else { f64::INFINITY };

    Ok(Outcome {
        aac_bytes: aac.len(),
        frames,
        snr_db,
        reported_delay: reported,
        measured_delay: measured,
    })
}

/// The offset, in frames, that best lines the decode up with the source.
///
/// Searched around `guess` rather than from zero: the reported delay is the right
/// neighbourhood even when it is not the right number, and a ±4096-frame window around it
/// costs a fraction of a full search. Compared on one channel over a bounded window, which is
/// ample — a few tenths of a second of audio separates a correct alignment from a wrong one
/// unmistakably.
fn best_lag(source: &Wav, decoded: &[i16], channels: usize, guess: usize) -> usize {
    const WINDOW: usize = 1 << 15;
    let reference: Vec<f64> = source.samples.iter().step_by(channels).map(|&s| s as f64).collect();
    let output: Vec<f64> = decoded.iter().step_by(channels).map(|&s| s as f64).collect();

    let low = guess.saturating_sub(4096);
    let high = (guess + 4096).min(output.len().saturating_sub(WINDOW / 2));
    let mut best = (guess, f64::NEG_INFINITY);

    for lag in low..=high {
        let n = (output.len() - lag).min(reference.len()).min(WINDOW);
        if n < 1024 {
            break;
        }
        // Unnormalised cross-correlation is enough here: every candidate compares the same
        // reference window, so the divisor would be a constant across the search.
        let dot: f64 = (0..n).map(|i| reference[i] * output[lag + i]).sum();
        let energy: f64 = (0..n).map(|i| output[lag + i].powi(2)).sum::<f64>().sqrt();
        let score = if energy > 0.0 { dot / energy } else { 0.0 };
        if score > best.1 {
            best = (lag, score);
        }
    }
    best.0
}

fn print_listening_notes(stem: &str, out_dir: &Path) {
    println!("\nwritten to {}/:", out_dir.display());
    println!("  {stem}-original.wav      the input, so the A/B is against a file");
    println!("  {stem}-<setting>.aac     the real thing — plays in ffplay, VLC, Safari, Chrome");
    println!("  {stem}-<setting>.wav     the same stream decoded back, time-aligned");
    println!("  {stem}-<setting>-diff.wav  what AAC discarded, gained up so it is audible");
    println!("\n  ffplay -autoexit out/{stem}-64k.aac");
}

/// A ten-second clip with something in it worth listening to: a bass line, a chord, and
/// hi-hat-like transients, which is roughly where a codec's decisions become audible.
fn demo_clip() -> Wav {
    const RATE: u32 = 44_100;
    let seconds = 10.0;
    let n = (RATE as f64 * seconds) as usize;
    let mut left = Vec::with_capacity(n);
    let mut right = Vec::with_capacity(n);
    let mut noise_state = 0.0f64;
    let mut rng: u64 = 0x1234_5678_9ABC_DEF0;

    for i in 0..n {
        let t = i as f64 / RATE as f64;
        // A bass note that changes every two seconds.
        let bass_hz = [55.0, 65.4, 73.4, 82.4, 55.0][((t / 2.0) as usize).min(4)];
        let bass = (t * bass_hz * std::f64::consts::TAU).sin() * 0.35;

        // A chord above it.
        let chord: f64 = [220.0, 277.2, 329.6]
            .iter()
            .map(|hz| (t * hz * std::f64::consts::TAU).sin())
            .sum::<f64>()
            * 0.12;

        // Hi-hats: filtered noise in short bursts, twice a second. Transients are what a
        // codec's block-switching decisions are for, and where a bad one is easiest to hear.
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let white = ((rng >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
        noise_state = noise_state * 0.35 + white * 0.65;
        let beat = (t * 2.0).fract();
        let envelope = if beat < 0.08 { (1.0 - beat / 0.08).powi(3) } else { 0.0 };
        let hat = noise_state * envelope * 0.3;

        // A little stereo width: the chord panned one way, the hats the other. Not a delay —
        // a delay between channels puts the mono downmix into partial cancellation, which is
        // exactly what Parametric Stereo cannot recover from, and `--profiles` would then
        // blame HE-AAC v2 for the test signal's problem.
        let sample = bass + chord + hat;
        left.push(sample + chord * 0.25 - hat * 0.2);
        right.push(sample - chord * 0.25 + hat * 0.2);
    }

    let mut samples = Vec::with_capacity(n * 2);
    for i in 0..n {
        samples.push((left[i] * 20000.0).clamp(-32768.0, 32767.0) as i16);
        samples.push((right[i] * 20000.0).clamp(-32768.0, 32767.0) as i16);
    }
    Wav { rate: RATE, channels: 2, samples }
}
