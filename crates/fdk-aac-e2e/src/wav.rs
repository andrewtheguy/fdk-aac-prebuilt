//! A minimal 16-bit PCM WAV writer.
//!
//! Why the audio is written as WAV rather than as the raw `i16` buffers it already is: the
//! same files serve two readers. The pipeline's cross-architecture SNR job parses them with
//! Python's standard-library `wave` module, which needs no dependency on a CI runner; and a
//! person can double-click them. A headerless `.pcm` would need the rate and channel count
//! carried out of band for the first and converted for the second, and the header costs 44
//! bytes.
//!
//! Deliberately not a general writer. 16-bit, PCM, any rate, any channel count — which is
//! everything this binary produces, and anything else should fail to compile rather than be
//! silently mis-described by a header claiming otherwise.

use std::io;
use std::path::Path;

/// Write interleaved 16-bit samples as a canonical 44-byte-header WAV.
///
/// Every multi-byte field is little-endian by the format's definition, so the conversions are
/// explicit rather than `to_ne_bytes` — on a big-endian host the native form would produce a
/// file that is wrong in a way nothing here would notice.
pub fn write(path: &Path, rate: u32, channels: u16, samples: &[i16]) -> io::Result<()> {
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let byte_rate = rate * u32::from(block_align);
    let data_bytes = samples.len() * 2;
    // The RIFF size field counts everything after it: the 4-byte "WAVE" tag, the 24-byte fmt
    // chunk with its header, and the 8-byte data chunk header, plus the samples.
    let riff_size = 4 + 24 + 8 + data_bytes;

    let mut out = Vec::with_capacity(44 + data_bytes);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk body is 16 bytes
    out.extend_from_slice(&1u16.to_le_bytes()); // format 1 = uncompressed PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out)
}
