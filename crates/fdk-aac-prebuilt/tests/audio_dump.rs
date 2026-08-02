//! Render every signal this suite tests with, and its round trip, as WAV files to listen to.
//!
//! `#[ignore]` because it is not a test — it asserts almost nothing and writes several
//! megabytes. It exists because the rest of the suite reports its findings as correlations,
//! and a correlation of 0.31 for band-limited noise is either "healthy, AAC codes noise by
//! preserving band energy rather than the waveform" or "the codec is broken", and the number
//! alone cannot tell you which. Listening can.
//!
//! ```sh
//! cargo test -p fdk-aac-prebuilt --test audio_dump -- --ignored --nocapture
//! ```
//!
//! Writes to `audio/tests/` at the repository root, or to `$FDK_AAC_AUDIO_DIR`.
//!
//! The signals come from `common`, the same functions the real tests call, rather than being
//! regenerated here. A renderer with its own copy of the generators could produce something
//! perfectly pleasant that no test has ever encoded.

mod common;

use common::{best_lag, to_f64, write_wav, Signal};
use fdk_aac::dec::{Decoder, Transport as DecTransport};
use fdk_aac::enc::{
    AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Transport as EncTransport,
};
use std::path::PathBuf;

const RATE: u32 = 48_000;

fn output_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FDK_AAC_AUDIO_DIR") {
        return PathBuf::from(dir);
    }
    // CARGO_MANIFEST_DIR is this crate; the repository root is two levels above it.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../audio/tests")
}

/// Encode and decode back, returning the decoded interleaved PCM.
fn round_trip(
    pcm: &[i16],
    rate: u32,
    channels: ChannelMode,
    aot: AudioObjectType,
    bitrate: u32,
) -> Vec<i16> {
    let count = channels.count();
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(bitrate),
        sample_rate: rate,
        transport: EncTransport::Adts,
        channels,
        audio_object_type: aot,
    })
    .expect("configuration must be encodable");

    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize * count;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();
    for chunk in pcm.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        stream.extend_from_slice(&out[..r.output_size]);
    }
    // Flushed, so the tail of the signal is actually in the file being listened to.
    while let Some(r) = encoder.flush(&mut out).unwrap() {
        if r.output_size == 0 {
            break;
        }
        stream.extend_from_slice(&out[..r.output_size]);
    }

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    decoder.fill(&stream).unwrap();
    let mut decoded = Vec::new();
    let mut buffer = vec![0i16; 16384];
    while decoder.decode_frame(&mut buffer).is_ok() {
        let n = decoder.decoded_frame_size();
        if n == 0 {
            break;
        }
        decoded.extend_from_slice(&buffer[..n]);
    }
    decoded
}

/// What AAC threw away, on its own and gained up until it is audible.
///
/// The most informative file of the set. The decoded audio mostly sounds like the input, so
/// the interesting question is not what survived but what did not, and at a comfortable
/// bitrate that is inaudible until it is isolated and amplified.
fn difference(reference: &[i16], decoded: &[i16], channels: usize) -> Vec<i16> {
    // Aligned first: the codec delay is a few thousand samples, and subtracting without
    // removing it produces the sum of two copies of the signal rather than the error.
    let left: Vec<f64> = decoded.iter().step_by(channels).map(|&s| s as f64).collect();
    let reference_left: Vec<f64> = reference.iter().step_by(channels).map(|&s| s as f64).collect();
    let (lag, _) = best_lag(&reference_left, &left, 12_000);

    let offset = lag * channels;
    let n = decoded.len().saturating_sub(offset).min(reference.len());
    (0..n)
        .map(|i| {
            let error = decoded[offset + i] as i32 - reference[i] as i32;
            // ×8, which is about 18 dB. Enough to hear the quantisation noise on the tonal
            // signals without clipping the noise one, where the error is nearly the signal.
            (error * 8).clamp(i16::MIN as i32, i16::MAX as i32) as i16
        })
        .collect()
}

/// Every signal class, its round trip, and the difference between them.
#[test]
#[ignore = "writes WAV files to listen to; run with --ignored"]
fn dump_every_signal_class() {
    let dir = output_dir();
    let samples = RATE as usize * 3;
    println!("writing to {}", dir.display());

    for signal in [
        common::tone(RATE, samples),
        common::chord(RATE, samples),
        common::sweep(RATE, samples),
        common::noise(RATE, samples),
    ] {
        let Signal { name, samples: source, corr_floor } = signal;
        let slug = name.replace(' ', "-");
        write_wav(&dir.join(format!("{slug}-source.wav")), RATE, 1, &source);

        // Three bitrates, because the point of listening is to hear where it stops being
        // transparent, and that is a different place for each of these four signals.
        for bitrate in [32_000u32, 64_000, 128_000] {
            let kbps = bitrate / 1000;
            let decoded = round_trip(
                &source,
                RATE,
                ChannelMode::Mono,
                AudioObjectType::Mpeg4LowComplexity,
                bitrate,
            );
            write_wav(&dir.join(format!("{slug}-{kbps}k.wav")), RATE, 1, &decoded);
            write_wav(
                &dir.join(format!("{slug}-{kbps}k-diff.wav")),
                RATE,
                1,
                &difference(&source, &decoded, 1),
            );
        }

        let decoded = round_trip(
            &source,
            RATE,
            ChannelMode::Mono,
            AudioObjectType::Mpeg4LowComplexity,
            128_000,
        );
        let (_, corr) = best_lag(&to_f64(&source), &to_f64(&decoded), 12_000);
        println!("  {name:>12}: {corr:.4} at 128 kbps (suite floor {corr_floor:.2})");
    }
}

/// The two HE profiles against plain AAC-LC at a bitrate low enough for the difference to be
/// the point — this is SBR being audible rather than SBR being a granule size in a table.
///
/// The **sweep**, and that choice is the whole exercise. SBR restores a top octave that AAC-LC
/// spends its bits away from, so demonstrating it needs a signal that has a top octave: this
/// dump first used `chord`, which is four partials between 220 and 440 Hz and measures −91 dB
/// above 13 kHz *before encoding*. All three profiles came back identical at −91 dB, which
/// looked like SBR doing nothing and was really a source with nothing for it to do. The sweep
/// runs to a quarter of the sample rate and measures −23.2 dB in the same band.
#[test]
#[ignore = "writes WAV files to listen to; run with --ignored"]
fn dump_the_he_profiles() {
    let dir = output_dir();
    let samples = RATE as usize * 3;
    let source = common::sweep(RATE, samples).samples;
    // Stereo, because HE-AAC v2's Parametric Stereo has nothing to do with a mono input.
    let stereo: Vec<i16> = source.iter().flat_map(|&s| [s, s]).collect();
    write_wav(&dir.join("profiles-source.wav"), RATE, 2, &stereo);

    for (label, aot) in [
        ("aaclc", AudioObjectType::Mpeg4LowComplexity),
        ("heaac", AudioObjectType::Mpeg4HeAac),
        ("heaacv2", AudioObjectType::Mpeg4HeAacV2),
    ] {
        let decoded = round_trip(&stereo, RATE, ChannelMode::Stereo, aot, 32_000);
        write_wav(&dir.join(format!("profiles-{label}-32k.wav")), RATE, 2, &decoded);
        println!("  {label:>8}: {} samples at 32 kbps stereo", decoded.len());
    }
}
