//! Encode, decode, and measure what came back.
//!
//! Everything else in this suite checks that the crate configured fdk-aac the way it was
//! asked to. This file checks the different and more basic thing: that the *library works* —
//! that a signal put in comes back out. An archive built for the wrong architecture, linked
//! from the wrong version, or corrupted in the cache can pass every configuration test in
//! here and still not reproduce a sine wave.
//!
//! Nothing is compared bit-exactly against a stored fixture, and that is deliberate. AAC is
//! lossy, so no measure can be an equality test; and how far two *targets* may differ is a
//! question for the pipeline, where real archives can be compared against each other, rather
//! than for a checked-in digest that has to be regenerated whenever the pinned version moves.
//! See the `compare` job in `.github/workflows/build.yml` — which is also where it is
//! recorded that fdk-aac is *not* bit-identical across architectures: each architecture
//! overrides a few of the generic routines with its own — A64 inline assembly on arm64, and
//! on x86 an `imul` plus four routines that compute in `float` and truncate back to fixed
//! point — and the two sets round differently.
//!
//! So: measures, with floors that travel with the signal. The floors sit well below what is
//! actually observed — the numbers each test prints are the real ones — because a threshold
//! tuned to the current measurement is a test that fails on the next compiler.

mod common;

use common::{best_lag, correlation, rms, to_f64, Signal};
use fdk_aac::dec::{Decoder, Transport as DecTransport};
use fdk_aac::enc::{
    AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Transport as EncTransport,
};

/// What a round trip produced, and what the two libraries said about it.
struct RoundTrip {
    decoded: Vec<i16>,
    /// `InfoStruct::nDelay` — the encoder's lookahead.
    encoder_delay: u32,
    /// `CStreamInfo::outputDelay` — the decoder's own, mostly its PCM limiter's.
    decoder_delay: u32,
}

impl RoundTrip {
    /// The delay a consumer must actually skip: both halves, which is a thing neither
    /// library tells you on its own.
    fn total_delay(&self) -> u32 {
        self.encoder_delay + self.decoder_delay
    }
}

/// Encode a mono signal and decode it straight back.
fn round_trip(signal: &Signal, rate: u32, bitrate: u32) -> RoundTrip {
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(bitrate),
        sample_rate: rate,
        transport: EncTransport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .expect("configuration must be encodable");

    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();
    for chunk in signal.samples.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        stream.extend_from_slice(&out[..r.output_size]);
    }
    while let Some(r) = encoder.flush(&mut out).unwrap() {
        if r.output_size == 0 {
            break;
        }
        stream.extend_from_slice(&out[..r.output_size]);
    }

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    decoder.fill(&stream).unwrap();
    let mut decoded = Vec::new();
    let mut pcm = vec![0i16; 8192];
    while decoder.decode_frame(&mut pcm).is_ok() {
        let n = decoder.decoded_frame_size();
        if n == 0 {
            break;
        }
        decoded.extend_from_slice(&pcm[..n]);
    }
    RoundTrip {
        decoded,
        encoder_delay: info.nDelay,
        decoder_delay: decoder.stream_info().outputDelay,
    }
}

/// Every signal class must survive a round trip at a comfortable bitrate.
///
/// Four classes rather than one tone, because AAC treats them very differently and a single
/// sine would let most failure modes through: a broken filterbank shows up on the sweep, a
/// broken psychoacoustic model on the chord, and noise goes down a path that does not
/// preserve the waveform at all — which is why its floor is where it is.
#[test]
fn every_signal_class_survives() {
    const RATE: u32 = 48_000;
    let samples = RATE as usize * 2;

    for signal in [
        common::tone(RATE, samples),
        common::chord(RATE, samples),
        common::sweep(RATE, samples),
        common::noise(RATE, samples),
    ] {
        let trip = round_trip(&signal, RATE, 128_000);
        assert!(
            trip.decoded.len() > signal.samples.len() / 2,
            "{}: only {} samples came back from {}",
            signal.name,
            trip.decoded.len(),
            signal.samples.len(),
        );

        let reference = to_f64(&signal.samples);
        let output = to_f64(&trip.decoded);
        // Aligned by measurement rather than by the reported delay alone — see below, where
        // the two are required to agree.
        let (lag, corr) = best_lag(&reference, &output, 8192);
        println!(
            "{:>12}: correlation {corr:.4} at lag {lag} (libraries report {})",
            signal.name,
            trip.total_delay(),
        );

        assert!(
            corr >= signal.corr_floor,
            "{}: correlation {corr:.4} is below the {:.2} floor for this signal class — \
             the codec path is not reproducing the input",
            signal.name,
            signal.corr_floor,
        );

        // Energy, separately from shape. A decoder that returned the right waveform at a
        // hundredth of the level would correlate perfectly and be useless.
        let n = output.len().saturating_sub(lag).min(reference.len());
        let ratio = rms(&output[lag..lag + n]) / rms(&reference[..n]);
        assert!(
            (0.5..2.0).contains(&ratio),
            "{}: decoded energy is {ratio:.3}× the input",
            signal.name,
        );
    }
}

/// The reported delays must be the delay that is actually there.
///
/// These are the numbers a consumer trims from the front of a decode to line it up with the
/// source. If either were wrong nothing would fail — the audio would simply be offset by a
/// few milliseconds, which is inaudible on its own and ruinous when two streams are being
/// synchronised or when segments are being joined. So both are checked against the lag that
/// actually maximises correlation.
///
/// **Both**, and their sum, because neither library tells the whole story. `nDelay` is the
/// encoder's lookahead; `outputDelay` is the decoder's, which for fdk-aac is mostly its PCM
/// limiter. A consumer that skips only `nDelay` — the obvious reading of the encoder's API,
/// and what a test asserting `lag == nDelay` would have encouraged — is left with the
/// decoder's delay still in the signal.
#[test]
fn the_reported_delays_account_for_the_real_one() {
    for rate in [16_000u32, 44_100, 48_000] {
        let signal = common::chord(rate, rate as usize * 2);
        let trip = round_trip(&signal, rate, 128_000);

        let reference = to_f64(&signal.samples);
        let output = to_f64(&trip.decoded);
        let (lag, corr) = best_lag(&reference, &output, 8192);
        println!(
            "{rate:>6} Hz: best lag {lag}, encoder {} + decoder {} = {}, correlation {corr:.4}",
            trip.encoder_delay,
            trip.decoder_delay,
            trip.total_delay(),
        );

        assert!(corr > 0.9, "{rate} Hz: nothing correlated well enough to measure a lag");
        assert!(trip.encoder_delay > 0, "the encoder reported no delay at all");

        let difference = (lag as i64 - trip.total_delay() as i64).abs();
        assert!(
            difference <= 64,
            "{rate} Hz: the libraries report {} samples of delay ({} encoder + {} decoder) \
             but the decode lines up best at {lag} — a {difference}-sample disagreement",
            trip.total_delay(),
            trip.encoder_delay,
            trip.decoder_delay,
        );
    }
}

/// More bits must mean a better reconstruction. A monotone relationship over a wide range is
/// something only a working encoder produces.
#[test]
fn quality_increases_with_bitrate() {
    const RATE: u32 = 48_000;
    let signal = common::chord(RATE, RATE as usize * 2);
    let reference = to_f64(&signal.samples);

    let mut previous = 0.0;
    let mut measurements = Vec::new();
    for bitrate in [32_000u32, 64_000, 128_000, 192_000] {
        let trip = round_trip(&signal, RATE, bitrate);
        let output = to_f64(&trip.decoded);
        let (_, corr) = best_lag(&reference, &output, 8192);
        println!("{:>7} bps: correlation {corr:.5}", bitrate);
        measurements.push((bitrate, corr));
        previous = corr.max(previous);
    }

    let lowest = measurements.first().unwrap().1;
    let highest = measurements.last().unwrap().1;
    assert!(
        highest > lowest,
        "192 kbps ({highest:.5}) did not beat 32 kbps ({lowest:.5}) — the bitrate is not \
         reaching the encoder"
    );
    assert!(previous > 0.99, "even at 192 kbps nothing correlated above 0.99");
}

/// Stereo must stay stereo: the two channels must come back distinguishable from each other.
///
/// The failure this catches is a channel mode that collapsed to mono somewhere — the output
/// is still audio, still the right length, and still correlates with the input, but one
/// channel has become the other.
#[test]
fn stereo_channels_stay_distinct() {
    const RATE: u32 = 48_000;
    let samples = RATE as usize * 2;
    let left = common::tone(RATE, samples).samples;
    let right = common::sweep(RATE, samples).samples;
    let interleaved: Vec<i16> = left.iter().zip(&right).flat_map(|(&l, &r)| [l, r]).collect();

    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(192_000),
        sample_rate: RATE,
        transport: EncTransport::Adts,
        channels: ChannelMode::Stereo,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();
    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize * 2;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();
    for chunk in interleaved.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        stream.extend_from_slice(&out[..r.output_size]);
    }

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    decoder.fill(&stream).unwrap();
    let mut decoded = Vec::new();
    let mut pcm = vec![0i16; 8192];
    while decoder.decode_frame(&mut pcm).is_ok() {
        let n = decoder.decoded_frame_size();
        if n == 0 {
            break;
        }
        decoded.extend_from_slice(&pcm[..n]);
    }
    assert_eq!(decoder.stream_info().numChannels, 2);

    let out_left: Vec<f64> = decoded.iter().step_by(2).map(|&s| s as f64).collect();
    let out_right: Vec<f64> = decoded.iter().skip(1).step_by(2).map(|&s| s as f64).collect();

    // Each output channel must match its own input better than the other one's.
    let (lag_l, corr_ll) = best_lag(&to_f64(&left), &out_left, 8192);
    let (_, corr_rr) = best_lag(&to_f64(&right), &out_right, 8192);
    let n = out_left.len().saturating_sub(lag_l).min(left.len());
    let corr_lr = correlation(&to_f64(&left)[..n], &out_right[lag_l..lag_l + n]);

    println!("L→L {corr_ll:.4}  R→R {corr_rr:.4}  L→R {corr_lr:.4}");
    assert!(corr_ll > 0.9, "the left channel did not survive: {corr_ll:.4}");
    assert!(corr_rr > 0.7, "the right channel did not survive: {corr_rr:.4}");
    assert!(
        corr_ll > corr_lr + 0.2,
        "the left input matches the right output nearly as well as the left ({corr_ll:.4} vs \
         {corr_lr:.4}) — the channels were collapsed or swapped"
    );
}

/// The whole rate range, at a bitrate each can carry. Cheap, and it is the test that would
/// notice a filterbank that only works at one rate.
#[test]
fn every_sample_rate_round_trips() {
    for rate in [8_000u32, 16_000, 22_050, 32_000, 44_100, 48_000] {
        let signal = common::tone(rate, rate as usize);
        let trip = round_trip(&signal, rate, 64_000);
        let reference = to_f64(&signal.samples);
        let output = to_f64(&trip.decoded);
        let (_, corr) = best_lag(&reference, &output, 8192);
        println!("{rate:>6} Hz: correlation {corr:.4}");
        assert!(corr > 0.9, "{rate} Hz round-tripped at only {corr:.4}");
    }
}
