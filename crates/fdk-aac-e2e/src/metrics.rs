//! Ways to compare two signals that are *supposed* to differ.
//!
//! AAC is lossy, so nothing here can be an equality test — the equality claim this binary
//! makes lives in the SHA-256 digests, which compare one target's output against another's
//! rather than against the input.
//!
//! What these measure instead is whether the decoded audio is the audio that went in.
//! Correlation is insensitive to gain, so `rms` covers the level separately; a decoder
//! returning the right waveform at a hundredth of the amplitude would score perfectly on the
//! first and be caught by the second.

/// Pearson correlation.
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

/// The delay, in samples, that best lines the decode up with the reference.
///
/// Searched rather than taken from `InfoStruct::nDelay`, because the total delay is the
/// encoder's *plus* the decoder's `outputDelay`, and for HE-AAC the two are quoted at
/// different sample rates. The test suite asserts that the searched answer and the reported
/// numbers agree; here the searched one is simply used.
///
/// Bounded to `WINDOW` samples of overlap: every candidate lag costs a pass otherwise, and a
/// fraction of a second is far more than enough to separate a correct alignment from a wrong
/// one.
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

pub fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}
