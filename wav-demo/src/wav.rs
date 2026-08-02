//! Minimal 16-bit PCM WAV reading and writing.
//!
//! No dependency, because this directory is a sample and a sample that pulls in a crate to
//! read a 44-byte header teaches the wrong lesson about what the codec needs.
//!
//! The reader walks the RIFF chunk list rather than assuming the header is 44 bytes, so
//! files with a `LIST`/`INFO` chunk before `data` — which is most files any real tool wrote
//! — work.

use std::path::Path;

pub struct Wav {
    pub rate: u32,
    pub channels: usize,
    /// Interleaved.
    pub samples: Vec<i16>,
}

impl Wav {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.rate as f64
    }
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

pub fn read(path: &Path) -> Result<Wav, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()));
    }

    let mut offset = 12;
    let mut format: Option<(u32, usize, u16)> = None;
    let mut samples: Option<Vec<i16>> = None;

    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size = u32le(&data[offset + 4..offset + 8]) as usize;
        let body = offset + 8;
        if body + size > data.len() {
            break;
        }

        match id {
            b"fmt " if size >= 16 => {
                let chunk = &data[body..body + size];
                let tag = u16le(&chunk[0..2]);
                // 1 is PCM; 0xFFFE is WAVE_FORMAT_EXTENSIBLE, which is what anything writing
                // more than two channels produces and which is still PCM underneath.
                if tag != 1 && tag != 0xFFFE {
                    return Err(format!("{} is not PCM (format tag {tag})", path.display()));
                }
                format = Some((
                    u32le(&chunk[4..8]),
                    u16le(&chunk[2..4]) as usize,
                    u16le(&chunk[14..16]),
                ));
            }
            b"data" => {
                samples = Some(
                    data[body..body + size]
                        .chunks_exact(2)
                        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                        .collect(),
                );
            }
            _ => {}
        }

        // Chunks are word-aligned: an odd size is followed by a pad byte that is not counted
        // in the size field. Forgetting this reads the next chunk's id one byte late.
        offset = body + size + (size & 1);
    }

    let (rate, channels, bits) = format.ok_or("no fmt chunk")?;
    let samples = samples.ok_or("no data chunk")?;
    if bits != 16 {
        return Err(format!("{}-bit input; this demo reads 16-bit PCM only", bits));
    }
    if channels == 0 || channels > 2 {
        return Err(format!("{channels} channels; this demo handles mono and stereo"));
    }
    Ok(Wav { rate, channels, samples })
}

pub fn write(path: &Path, wav: &Wav) -> Result<(), String> {
    let bytes_per_frame = wav.channels * 2;
    let data_len = wav.samples.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&(wav.channels as u16).to_le_bytes());
    out.extend_from_slice(&wav.rate.to_le_bytes());
    out.extend_from_slice(&(wav.rate * bytes_per_frame as u32).to_le_bytes());
    out.extend_from_slice(&(bytes_per_frame as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in &wav.samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }

    std::fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}
