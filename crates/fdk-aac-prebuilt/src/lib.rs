//! Safe bindings for the Fraunhofer FDK AAC codec, linked from a prebuilt static archive.
//!
//! This is a fork of [`fdk-aac`](https://crates.io/crates/fdk-aac) 0.8.0 by Hailey
//! Somerville (MIT). The module layout, the type names and the method names are theirs, so
//! a project switching to this crate changes one line of its manifest:
//!
//! ```toml
//! fdk-aac = { package = "fdk-aac-prebuilt", git = "…", tag = "…" }
//! ```
//!
//! and keeps every `use fdk_aac::enc::…` untouched.
//!
//! # What is different, and why
//!
//! Unlike this repository's sibling [`libopus-prebuilt`], which keeps its safe wrapper
//! byte-identical to upstream, this fork changes things. Upstream 0.8.0 has defects that a
//! repository whose whole point is a *trustworthy* AAC build cannot ship unfixed:
//!
//! - **`Encoder::encode` took `&self`** while `aacEncEncode` mutates encoder state, and the
//!   handle carried an `unsafe impl Sync`. Two threads sharing an `&Encoder` was therefore
//!   a data race reachable from entirely safe code. `encode` now takes `&mut self` and
//!   [`enc::Encoder`] is `Send` but not `Sync`. This is the one change that can require an
//!   edit at a call site — `let mut encoder` instead of `let encoder`.
//! - **HE-AAC did not work.** Upstream set `AACENC_SBR_MODE` to 0 on every encoder,
//!   unconditionally, which *disables* Spectral Band Replication — so `Mpeg4HeAac` and
//!   `Mpeg4HeAacV2` produced plain AAC-LC while claiming otherwise. That parameter is
//!   documented as being for ELD only and defaults to auto; this fork does not touch it,
//!   and the audio object type decides, as fdk-aac intends.
//! - **The encoder was opened with a hardcoded two channels.** Now derived from the
//!   requested [`enc::ChannelMode`].
//! - **Only ADTS and raw transports existed**, spelled as bare `0` and `2` rather than the
//!   `TRANSPORT_TYPE` constants. LOAS, LATM and ADIF are reachable now, on both sides.
//! - **No parameter access at all** beyond the five fields of `EncoderParams`. Both
//!   [`enc::Encoder::set_param`] and [`dec::Decoder::set_param`] now reach the whole
//!   documented set — afterburner, bandwidth, signalling mode, peak bitrate, concealment,
//!   the output limiter.
//!
//! Everything else — the error types, their message tables, `EncodeInfo`, `InfoStruct`,
//! `StreamInfo` — is upstream's, and stays that way so that a project written against
//! upstream's semantics finds them here.
//!
//! [`libopus-prebuilt`]: https://github.com/andrewtheguy/libopus-prebuilt
//!
//! # Example
//!
//! ```no_run
//! use fdk_aac::enc::{Encoder, EncoderParams, BitRate, ChannelMode, AudioObjectType, Transport};
//!
//! let mut encoder = Encoder::new(EncoderParams {
//!     bit_rate: BitRate::Cbr(32_000),
//!     sample_rate: 16_000,
//!     transport: Transport::Adts,
//!     channels: ChannelMode::Mono,
//!     audio_object_type: AudioObjectType::Mpeg4LowComplexity,
//! })?;
//!
//! // fdk-aac wants exactly one frame of interleaved i16 per call; `frameLength` says how
//! // many samples that is, and `nDelay` is the encoder delay a player has to skip.
//! let info = encoder.info()?;
//! let pcm = vec![0i16; info.frameLength as usize];
//! let mut adts = vec![0u8; info.maxOutBufBytes as usize];
//! let written = encoder.encode(&pcm, &mut adts)?;
//! # Ok::<(), fdk_aac::enc::EncoderError>(())
//! ```

pub mod dec;
pub mod enc;

/// Which fdk-aac these bindings were generated against, and which library versions the
/// linked archive must report.
///
/// Re-exported from the `-sys` crate so that a consumer can assert it is linking what it
/// thinks it is without depending on the `-sys` crate directly.
pub use fdk_aac_prebuilt_sys::version;
