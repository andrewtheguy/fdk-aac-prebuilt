//! Shared machinery for the integration tests: test signals, the measures used to compare
//! them, and an ADTS header parser.
//!
//! The parser is the important one and is worth justifying. Most of what can go wrong in
//! this crate is a *wrong integer sent to the C library* — a transposed `AACENC_PARAM`, a
//! `TRANSPORT_TYPE` off by one, a channel mode that means something else. None of those is
//! a compile error, and none of them makes `encode` return an error either: fdk-aac happily
//! encodes with the wrong sample rate index and hands back bytes. The only thing that
//! catches it is reading the header back out of the bitstream and checking it says what was
//! asked for, which is what `Adts::parse` is for.
//!
//! Everything here is written against the bitstream and the arithmetic, with no dependency
//! on fdk-aac, so a test failure points at the encoder rather than at the harness.

#![allow(dead_code)] // each test binary uses a different subset

use std::f64::consts::TAU;

// ---------------------------------------------------------------- signals

/// A deterministic PRNG, so the noise is the same noise on every target and every run.
///
/// `f64::sin` below is not bit-identical across platforms — glibc dispatches it per CPU —
/// but every comparison here is approximate, and a last-place-bit difference in the input
/// cannot move a correlation by a measurable amount. Where bit-exactness *is* the claim, it
/// is asserted on the encoded bitstream from integer input, not on anything float.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// A test signal, and the correlation a healthy round trip of it must still show.
///
/// Four classes rather than one tone, because AAC behaves very differently depending on
/// what it is given and a single sine would let most failure modes through. The floor
/// travels with the signal rather than being one global constant for the same reason it
/// does in the Opus repository: a perceptual codec discards the parts of noise-like content
/// that a waveform comparison is most sensitive to, so a healthy encoder round-trips noise
/// at a correlation that would be alarming for a chord. One global threshold would either
/// pass a broken tonal path or fail a working noise one.
pub struct Signal {
    pub name: &'static str,
    /// Mono, at the rate it was asked for, as i16.
    pub samples: Vec<i16>,
    /// The lowest correlation this class may show and still be considered working. Every
    /// value sits well below what is actually measured — see the comments on each.
    pub corr_floor: f64,
}

/// A pure tone. The easiest thing there is to code, so the floor is high: anything below
/// this means the codec is not working at all rather than working imperfectly.
pub fn tone(rate: u32, samples: usize) -> Signal {
    Signal {
        name: "440 Hz tone",
        samples: (0..samples)
            .map(|i| ((i as f64 / rate as f64 * 440.0 * TAU).sin() * 8000.0) as i16)
            .collect(),
        corr_floor: 0.95,
    }
}

/// Four harmonically related partials. Exercises the psychoacoustic model's masking
/// decisions in a way a single sine does not.
pub fn chord(rate: u32, samples: usize) -> Signal {
    Signal {
        name: "chord",
        samples: (0..samples)
            .map(|i| {
                let t = i as f64 / rate as f64;
                let v: f64 =
                    [220.0, 277.2, 329.6, 440.0].iter().map(|hz| (t * hz * TAU).sin()).sum();
                (v / 4.0 * 8000.0) as i16
            })
            .collect(),
        corr_floor: 0.9,
    }
}

/// A sweep from 100 Hz to a quarter of the sample rate. Moves across the whole filterbank,
/// which is where a broken transform shows up as a band that goes missing.
pub fn sweep(rate: u32, samples: usize) -> Signal {
    let end = rate as f64 / 4.0;
    Signal {
        name: "sweep",
        samples: (0..samples)
            .map(|i| {
                let progress = i as f64 / samples as f64;
                let hz = 100.0 * (end / 100.0f64).powf(progress);
                let t = i as f64 / rate as f64;
                ((t * hz * TAU).sin() * 8000.0) as i16
            })
            .collect(),
        corr_floor: 0.75,
    }
}

/// Band-limited noise. The hard case, and the reason the floor is per-signal: AAC codes
/// noise by preserving the energy in each band rather than the waveform, so a perfectly
/// healthy encoder returns something that sounds right and correlates badly.
pub fn noise(rate: u32, samples: usize) -> Signal {
    let mut rng = Rng::new(0x5EED);
    // A one-pole lowpass, so the content is noise but not white — white noise at 8000
    // amplitude is mostly above the encoder's bandwidth and would test the lowpass rather
    // than the codec.
    let mut state = 0.0;
    let _ = rate;
    Signal {
        name: "noise",
        samples: (0..samples)
            .map(|_| {
                state = state * 0.85 + rng.next_f64() * 0.15;
                (state * 20000.0) as i16
            })
            .collect(),
        corr_floor: 0.3,
    }
}

// ---------------------------------------------------------------- measures

/// Pearson correlation. Insensitive to overall gain, unlike SNR, which matters because AAC
/// does not promise to preserve absolute level through a decoder with a limiter in it.
pub fn correlation(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let ma = a[..n].iter().sum::<f64>() / n as f64;
    let mb = b[..n].iter().sum::<f64>() / n as f64;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da == 0.0 || db == 0.0 {
        return 0.0;
    }
    num / (da.sqrt() * db.sqrt())
}

/// The delay, in samples, that best lines the decode up with the reference, searched over
/// `range`.
///
/// `InfoStruct::nDelay` is the honest answer and the tests assert it is close to this — but
/// it is not usable as the *only* alignment, because the decoder adds its own delay on top
/// (`CStreamInfo::outputDelay`) and for HE-AAC the two are at different sample rates. So
/// the tests align by measurement and then check the measurement agrees with what the
/// library reported, which tests both.
/// Note for periodic signals: a pure tone correlates just as well at any whole number of
/// periods, so the lag this reports for one is the first of several equally good answers and
/// is not the delay. Tests that care about the delay use a signal with structure — a chord
/// or a sweep — which has one unambiguous alignment.
///
/// The search is bounded to `WINDOW` samples of overlap rather than the whole signal. Every
/// candidate lag costs a full pass otherwise, which made this the slowest thing in the suite
/// by an order of magnitude; a few seconds of audio is far more than enough to separate a
/// correct alignment from a wrong one, and CI runs these unoptimised.
pub fn best_lag(reference: &[f64], decoded: &[f64], range: usize) -> (usize, f64) {
    const WINDOW: usize = 32_768;
    let mut best = (0usize, f64::NEG_INFINITY);
    for lag in 0..=range {
        if lag >= decoded.len() {
            break;
        }
        let n = (decoded.len() - lag).min(reference.len()).min(WINDOW);
        if n < 256 {
            break;
        }
        let corr = correlation(&reference[..n], &decoded[lag..lag + n]);
        if corr > best.1 {
            best = (lag, corr);
        }
    }
    best
}

pub fn to_f64(pcm: &[i16]) -> Vec<f64> {
    pcm.iter().map(|&s| s as f64).collect()
}

pub fn rms(pcm: &[f64]) -> f64 {
    if pcm.is_empty() {
        return 0.0;
    }
    (pcm.iter().map(|s| s * s).sum::<f64>() / pcm.len() as f64).sqrt()
}

// ---------------------------------------------------------------- ADTS

/// The MPEG-4 sampling frequency index table, which is what an ADTS header carries instead
/// of a rate. A wrong `AACENC_SAMPLERATE` shows up here and nowhere else.
pub const SAMPLING_FREQUENCIES: [u32; 13] =
    [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

pub fn frequency_index(rate: u32) -> Option<u8> {
    SAMPLING_FREQUENCIES.iter().position(|&r| r == rate).map(|i| i as u8)
}

/// One parsed ADTS frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adts {
    /// 0 for MPEG-4, 1 for MPEG-2. Set by the audio object type.
    pub mpeg_version: u8,
    /// The AAC profile, as the two-bit ADTS field: 0 is Main, **1 is LC**, 2 is SSR.
    /// Note this is `object_type - 1`, which is the off-by-one the format is famous for.
    pub profile: u8,
    pub sampling_frequency_index: u8,
    pub channel_configuration: u8,
    /// Total frame length in bytes, header included.
    pub frame_length: usize,
    /// Whether a CRC follows the fixed header (so the header is 9 bytes, not 7).
    pub has_crc: bool,
}

impl Adts {
    /// Parse the header at the start of `data`, returning `None` if it is not one.
    ///
    /// Written out by hand from the ADTS field layout rather than pulled from a crate,
    /// because a parser that shares code with the thing it is testing tests nothing.
    pub fn parse(data: &[u8]) -> Option<Adts> {
        if data.len() < 7 {
            return None;
        }
        // syncword: 12 bits of 1.
        if data[0] != 0xFF || (data[1] & 0xF0) != 0xF0 {
            return None;
        }
        let mpeg_version = (data[1] >> 3) & 0x01;
        // layer, 2 bits, always 0 for AAC.
        if (data[1] >> 1) & 0x03 != 0 {
            return None;
        }
        let has_crc = (data[1] & 0x01) == 0;
        let profile = (data[2] >> 6) & 0x03;
        let sampling_frequency_index = (data[2] >> 2) & 0x0F;
        let channel_configuration = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);
        // 13 bits spanning three bytes, which is the part a hand-written parser gets wrong.
        let frame_length = (((data[3] as usize) & 0x03) << 11)
            | ((data[4] as usize) << 3)
            | ((data[5] as usize) >> 5);
        Some(Adts {
            mpeg_version,
            profile,
            sampling_frequency_index,
            channel_configuration,
            frame_length,
            has_crc,
        })
    }

    pub fn sample_rate(&self) -> Option<u32> {
        SAMPLING_FREQUENCIES.get(self.sampling_frequency_index as usize).copied()
    }
}

/// Walk a whole ADTS stream, returning every frame header.
///
/// Follows the `frame_length` field from frame to frame rather than scanning for
/// syncwords: a stream whose lengths are wrong then fails to parse, instead of being
/// silently resynchronised by a scan that hides the bug.
pub fn adts_frames(data: &[u8]) -> Vec<Adts> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset + 7 <= data.len() {
        let Some(header) = Adts::parse(&data[offset..]) else {
            break;
        };
        if header.frame_length < 7 || offset + header.frame_length > data.len() {
            break;
        }
        offset += header.frame_length;
        frames.push(header);
    }
    frames
}
