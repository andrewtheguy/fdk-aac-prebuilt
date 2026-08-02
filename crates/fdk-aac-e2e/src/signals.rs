//! Test signals, generated with **integer arithmetic only**.
//!
//! This constraint is the whole reason this file exists rather than reusing the generators
//! in the test suite, which are written with `f64::sin` and are the better choice there.
//!
//! The binary's headline claim is that the four published archives produce byte-identical
//! bitstreams, checked by comparing SHA-256 digests across four runners. That comparison is
//! only meaningful if the *input* is byte-identical too — and `f64::sin` is not. glibc
//! dispatches it per CPU, macOS ships a different implementation again, and the results
//! differ in the last place. One sample differing by one LSB is enough to change a
//! quantisation decision, which changes a bit, which changes the digest — and the pipeline
//! would report a cross-target mismatch caused entirely by libm.
//!
//! So: a phase accumulator, a triangle, and a parabola, all in `i32`. The result is not a
//! pure sine, and does not need to be. It is smooth, periodic, harmonically rich enough to
//! exercise the filterbank and the psychoacoustic model, and identical on every machine that
//! has ever run it.

/// One cycle of a smooth periodic wave, from a `u32` phase.
///
/// A triangle folded through a parabola: `y = t · (2 − |t|)`, which is the standard
/// cheap sine approximation. Peak error against a real sine is around 1.5%, entirely in the
/// harmonics, which for this purpose is a feature — it gives the encoder something above the
/// fundamental to spend bits on.
fn parabolic(phase: u32) -> i32 {
    // Phase to a triangle in Q15: rises from -32768 to 32767 over half a cycle, falls back
    // over the other half.
    let quarter = (phase >> 15) as i32 & 0x1FFFF; // 0..131071 over the cycle
    let t = if quarter < 65536 { quarter - 32768 } else { 98304 - quarter };
    // y = t * (2 - |t|) on a Q15 scale, which keeps everything inside i32.
    let folded = (t * (65536 - t.abs())) >> 15;
    folded.clamp(-32768, 32767)
}

/// A deterministic PRNG. Integer only, and the same sequence everywhere.
struct Rng(u32);

impl Rng {
    /// One step of a 32-bit xorshift. Chosen over an LCG because the low bits of an LCG are
    /// nearly periodic, and the low bits are what end up in the signal.
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// The signal every configuration is encoded from.
///
/// Three components, because each covers a different part of the encoder:
///
/// - a **fundamental** near 440 Hz, which the psychoacoustic model will protect and which
///   makes the round-trip correlation meaningful;
/// - a **fifth above it**, so there is a second tonal peak and the joint-stereo and
///   masking decisions have something to do;
/// - a small amount of **lowpassed noise**, so the spectrum is not purely tonal and the
///   noise-substitution paths are entered.
///
/// The frequencies are computed as integer phase increments from the sample rate, so they
/// land on slightly different exact frequencies at each rate — which is correct: the point
/// is a deterministic signal per rate, not the same frequency across rates.
///
/// `tilt` swaps the emphasis between the two tones, which is how the second channel is made
/// (see [`interleave`]).
fn generate(rate: u32, samples: usize, tilt: bool, seed: u32) -> Vec<i16> {
    // Q32 phase increment for f Hz: f · 2³² / rate, computed in u64 so it does not overflow.
    let increment = |hz: u64| ((hz << 32) / rate as u64) as u32;
    let fundamental = increment(440);
    let fifth = increment(660);
    let (weight_a, weight_b) = if tilt { (3, 5) } else { (5, 3) };

    let mut phase_a: u32 = 0;
    let mut phase_b: u32 = 0;
    let mut rng = Rng(seed);
    // A one-pole integer lowpass on the noise, so it sits under the tones rather than
    // dominating the spectrum the encoder sees.
    let mut filtered: i32 = 0;

    (0..samples)
        .map(|_| {
            phase_a = phase_a.wrapping_add(fundamental);
            phase_b = phase_b.wrapping_add(fifth);

            let noise = (rng.next() >> 16) as i32 - 32768;
            filtered += (noise - filtered) >> 3;

            // Divided by 20 rather than by the weights' sum of 9, which puts the peak at
            // roughly half of full scale. Deliberately not hot: a near-0 dBFS input would
            // put the decoder's PCM limiter to work, and the energy check in main.rs would
            // then be measuring the limiter rather than the codec.
            let sum =
                (parabolic(phase_a) * weight_a + parabolic(phase_b) * weight_b + filtered) / 20;
            sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16
        })
        .collect()
}

/// The reference signal, and the first channel of the stereo pair.
pub fn deterministic_mono(rate: u32, samples: usize) -> Vec<i16> {
    generate(rate, samples, false, 0x5EED_5EED)
}

/// Build an interleaved signal whose channels are related but not identical.
///
/// **Not a delayed copy, and this took a measurement to get right.** The obvious way to make
/// a second channel is to delay the first — real stereo has interchannel delay, after all —
/// and it is wrong here. A 37-sample offset at 44.1 kHz is about 200° at 660 Hz, so the
/// *mono downmix* of the pair nearly cancels that tone. AAC-LC survives it, because it codes
/// left and right; Parametric Stereo does not, because it codes only the downmix plus a
/// handful of spatial parameters and cannot restore what cancelled before it saw it. The
/// symptom was an HE-AAC v2 round trip coming back 15 dB quiet while a plain tone through
/// the same encoder came back at unity — a badly chosen test signal wearing the appearance
/// of a broken codec.
///
/// So the channels differ in *content* rather than in phase: the same two tones with their
/// weights swapped, and independent noise. Nothing cancels in the downmix, the side channel
/// is substantial enough that a broken joint-stereo path would show, and every sample is
/// still integer-deterministic.
pub fn interleave(rate: u32, mono: &[i16], channels: usize) -> Vec<i16> {
    if channels == 1 {
        return mono.to_vec();
    }
    // Same rate and length as `mono`, tilted the other way, with its own noise seed.
    let other = generate(rate, mono.len(), true, 0x1234_ABCD);
    let mut out = Vec::with_capacity(mono.len() * channels);
    for (i, &sample) in mono.iter().enumerate() {
        out.push(sample);
        out.push(other[i]);
        for _ in 2..channels {
            out.push(sample);
        }
    }
    out
}
