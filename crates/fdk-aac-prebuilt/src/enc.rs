//! The AAC encoder.
//!
//! Descended from `fdk-aac` 0.8.0 by Hailey Somerville (MIT). See the crate documentation
//! for the list of things this fork changed and why; each of them is marked `FORK:` at the
//! point where it happens.

use std::cmp;
use std::fmt::{self, Debug, Display};
use std::mem::{self, MaybeUninit};
use std::os::raw::{c_int, c_uint, c_void};
use std::ptr;

use fdk_aac_prebuilt_sys as sys;

pub use sys::AACENC_InfoStruct as InfoStruct;

pub struct EncoderError(sys::AACENC_ERROR);

impl EncoderError {
    /// The raw `AACENC_ERROR` code, for callers matching on a specific condition rather
    /// than reporting one.
    ///
    /// FORK: upstream kept the code entirely private, so the only way to distinguish
    /// "the output buffer was too small" from "the configuration is invalid" was to compare
    /// message strings.
    pub fn code(&self) -> sys::AACENC_ERROR {
        self.0
    }

    /// True when fdk-aac reported end-of-file rather than a fault — which is what a flush
    /// call returns once the encoder has nothing left, and is not an error condition.
    pub fn is_eof(&self) -> bool {
        self.0 == sys::AACENC_ERROR_AACENC_ENCODE_EOF
    }

    pub fn message(&self) -> &'static str {
        match self.0 {
            sys::AACENC_ERROR_AACENC_OK => "Ok",
            sys::AACENC_ERROR_AACENC_INVALID_HANDLE => {
                "Handle passed to function call was invalid."
            }
            sys::AACENC_ERROR_AACENC_MEMORY_ERROR => "Memory allocation failed.",
            sys::AACENC_ERROR_AACENC_UNSUPPORTED_PARAMETER => "Parameter not available.",
            sys::AACENC_ERROR_AACENC_INVALID_CONFIG => "Configuration not provided.",
            sys::AACENC_ERROR_AACENC_INIT_ERROR => "General initialization error.",
            sys::AACENC_ERROR_AACENC_INIT_AAC_ERROR => "AAC library initialization error.",
            sys::AACENC_ERROR_AACENC_INIT_SBR_ERROR => "SBR library initialization error.",
            sys::AACENC_ERROR_AACENC_INIT_TP_ERROR => "Transport library initialization error.",
            sys::AACENC_ERROR_AACENC_INIT_META_ERROR => "Meta data library initialization error.",
            sys::AACENC_ERROR_AACENC_INIT_MPS_ERROR => "MPS library initialization error.",
            sys::AACENC_ERROR_AACENC_ENCODE_ERROR => {
                "The encoding process was interrupted by an unexpected error."
            }
            sys::AACENC_ERROR_AACENC_ENCODE_EOF => "End of file reached.",
            _ => "Unknown error",
        }
    }
}

impl Debug for EncoderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "EncoderError {{ code: {:?}, message: {:?} }}", self.0 as c_int, self.message())
    }
}

impl Display for EncoderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

// FORK: upstream implemented neither, so `?` could not lift an EncoderError into a
// `Box<dyn Error>` and no caller could use it with anyhow or thiserror without a wrapper.
impl std::error::Error for EncoderError {}

fn check(e: sys::AACENC_ERROR) -> Result<(), EncoderError> {
    if e == sys::AACENC_ERROR_AACENC_OK {
        Ok(())
    } else {
        Err(EncoderError(e))
    }
}

struct EncoderHandle {
    ptr: sys::HANDLE_AACENCODER,
}

// FORK: `Send` only.
//
// Upstream had `unsafe impl Sync for EncoderHandle` as well, and `Encoder::encode(&self)`.
// Together those make this compile, and race:
//
//     let encoder = Encoder::new(params)?;
//     std::thread::scope(|s| {
//         s.spawn(|| encoder.encode(&a, &mut x));
//         s.spawn(|| encoder.encode(&b, &mut y));
//     });
//
// `aacEncEncode` walks and rewrites the encoder's internal state — the bit reservoir, the
// psychoacoustic history, the transport framer's counters — so two concurrent calls corrupt
// each other's output at best. Moving an encoder to another thread is fine and useful, so
// `Send` stays.
unsafe impl Send for EncoderHandle {}

impl EncoderHandle {
    fn alloc(max_modules: usize, max_channels: usize) -> Result<Self, EncoderError> {
        let mut ptr: sys::HANDLE_AACENCODER = ptr::null_mut();
        check(unsafe {
            sys::aacEncOpen(&mut ptr as *mut _, max_modules as c_uint, max_channels as c_uint)
        })?;
        Ok(EncoderHandle { ptr })
    }
}

impl Drop for EncoderHandle {
    fn drop(&mut self) {
        unsafe {
            sys::aacEncClose(&mut self.ptr as *mut _);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitRate {
    /// Constant bit rate, in bits per second.
    Cbr(u32),
    /// Variable bit rate. The five modes are fdk-aac's own; the approximate stereo AAC-LC
    /// rates it documents for them are 32, 72, 112, 148 and 228 kbps respectively, and are
    /// much lower for mono.
    VbrVeryLow,
    VbrLow,
    VbrMedium,
    VbrHigh,
    VbrVeryHigh,
}

impl BitRate {
    /// The `AACENC_BITRATEMODE` value. 0 is CBR; 1..=5 are the VBR modes.
    fn mode(self) -> u32 {
        match self {
            BitRate::Cbr(_) => 0,
            BitRate::VbrVeryLow => 1,
            BitRate::VbrLow => 2,
            BitRate::VbrMedium => 3,
            BitRate::VbrHigh => 4,
            BitRate::VbrVeryHigh => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    Mono,
    Stereo,
}

impl ChannelMode {
    /// The `AACENC_CHANNELMODE` value, which for these two is simply the channel count.
    fn value(self) -> u32 {
        self.count() as u32
    }

    /// How many interleaved samples one frame of input holds per frame length.
    pub fn count(self) -> usize {
        match self {
            ChannelMode::Mono => 1,
            ChannelMode::Stereo => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioObjectType {
    /// MPEG-4 AAC Low Complexity. Value: 2
    Mpeg4LowComplexity,
    /// MPEG-4 AAC LC with Spectral Band Replication (HE-AAC). Value: 5
    Mpeg4HeAac,
    /// MPEG-4 AAC LC with SBR and Parametric Stereo (HE-AAC v2). Stereo input only.
    /// Value: 29
    Mpeg4HeAacV2,
    /// MPEG-4 AAC Low-Delay. Value: 23
    Mpeg4LowDelay,
    /// MPEG-4 AAC Enhanced Low-Delay. Value: 39
    Mpeg4EnhancedLowDelay,
    /// MPEG-2 AAC Low Complexity. Value: 129
    Mpeg2Aac,
    /// MPEG-2 AAC LC with Spectral Band Replication (HE-AAC). Value: 132
    Mpeg2HeAac,
}

impl AudioObjectType {
    fn value(self) -> sys::AUDIO_OBJECT_TYPE {
        match self {
            AudioObjectType::Mpeg4LowComplexity => sys::AUDIO_OBJECT_TYPE_AOT_AAC_LC,
            AudioObjectType::Mpeg4HeAac => sys::AUDIO_OBJECT_TYPE_AOT_SBR,
            AudioObjectType::Mpeg4HeAacV2 => sys::AUDIO_OBJECT_TYPE_AOT_PS,
            AudioObjectType::Mpeg4LowDelay => sys::AUDIO_OBJECT_TYPE_AOT_ER_AAC_LD,
            AudioObjectType::Mpeg4EnhancedLowDelay => sys::AUDIO_OBJECT_TYPE_AOT_ER_AAC_ELD,
            AudioObjectType::Mpeg2Aac => sys::AUDIO_OBJECT_TYPE_AOT_MP2_AAC_LC,
            AudioObjectType::Mpeg2HeAac => sys::AUDIO_OBJECT_TYPE_AOT_MP2_SBR,
        }
    }

    /// Whether this object type carries Spectral Band Replication, which is what makes the
    /// encoder's output sample rate twice its core rate.
    pub fn has_sbr(self) -> bool {
        matches!(
            self,
            AudioObjectType::Mpeg4HeAac
                | AudioObjectType::Mpeg4HeAacV2
                | AudioObjectType::Mpeg2HeAac
        )
    }
}

/// The bitstream format the encoder wraps its frames in.
///
/// FORK: upstream had `Raw` and `Adts` only, and mapped them to bare `0` and `2` written
/// out at the call site rather than to fdk-aac's `TRANSPORT_TYPE` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// No framing at all: bare access units. The configuration a decoder needs travels
    /// out-of-band, in `InfoStruct::confBuf` — this is the format to use inside MP4.
    Raw,
    /// ADTS. Self-framing, self-describing, and what a `.aac` file on disk contains; every
    /// frame carries a header, which costs 7 bytes each and is why it is not used in MP4.
    Adts,
    /// LOAS/LATM with AudioMuxElement framing — MPEG-2 transport streams and DAB+.
    Loas,
    /// LATM with a single MuxConfigPresent layer.
    Latm,
    /// ADIF: one header for the whole stream and no per-frame framing, so it cannot be
    /// seeked or joined mid-stream. Present for completeness.
    Adif,
}

impl Transport {
    fn transmux(self) -> u32 {
        // fdk-aac's own constants rather than the literals upstream wrote inline. A
        // transposed digit here selects a *different container*, which shows up as a
        // decoder that cannot sync rather than as anything that names this line.
        (match self {
            Transport::Raw => sys::TRANSPORT_TYPE_TT_MP4_RAW,
            Transport::Adts => sys::TRANSPORT_TYPE_TT_MP4_ADTS,
            Transport::Loas => sys::TRANSPORT_TYPE_TT_MP4_LOAS,
            Transport::Latm => sys::TRANSPORT_TYPE_TT_MP4_LATM_MCP1,
            Transport::Adif => sys::TRANSPORT_TYPE_TT_MP4_ADIF,
        }) as u32
    }
}

/// An encoder parameter, for [`Encoder::set_param`] and [`Encoder::get_param`].
///
/// FORK: upstream exposed no parameter access whatsoever, so the settings that decide
/// whether the output is any good — the afterburner above all — were unreachable.
///
/// The value stays a `u32` rather than becoming a per-parameter type. fdk-aac's own
/// interface is `(param, UINT)`, the meaning of the integer is documented per parameter in
/// `aacenc_lib.h`, and inventing a Rust enum for each would be a second thing to keep in
/// sync with a C header for no safety gained — the encoder validates the value and returns
/// `UNSUPPORTED_PARAMETER` for a bad one either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Param {
    /// Extra quality for extra time. Off by default in fdk-aac; the library's own
    /// documentation recommends turning it on for anything that is not real-time.
    Afterburner,
    /// Core encoder bandwidth in Hz. 0 lets the encoder choose from the bitrate, which is
    /// almost always what you want.
    Bandwidth,
    /// Ceiling for the bit reservoir in CBR mode, in bits per second.
    PeakBitrate,
    /// 1024 (the default) or 512/480 for the low-delay object types.
    GranuleLength,
    /// SBR on or off, for the ELD object type only. -1 (auto) is the default and is what
    /// this crate leaves it at — see the crate docs for why upstream setting it to 0 broke
    /// HE-AAC.
    SbrMode,
    /// The SBR sampling-rate ratio: 1 for downsampled SBR, 2 for dual-rate.
    SbrRatio,
    /// How the object type is signalled in the transport header: 0 implicit, 1 explicit
    /// backward-compatible, 2 explicit hierarchical. Matters for whether an HE-AAC stream
    /// plays as HE-AAC or as its AAC-LC core on an old decoder.
    ///
    /// Reading this back does not return what was set: fdk-aac runs it through
    /// `getSbrSignalingMode()`, which reports -1 (`0xFFFF_FFFF`) on any stream without SBR.
    SignalingMode,
    /// How often, in frames, the transport layer repeats its configuration. 0 means once.
    HeaderPeriod,
    /// Sub-frames per transport frame, for LATM and LOAS.
    TpSubframes,
    /// AudioMuxVersion for LATM/LOAS: 0, 1 or 2.
    AudioMuxVersion,
    /// Whether the transport layer adds a CRC. ADTS only.
    Protection,
    /// Bits per second reserved for ancillary data.
    AncillaryBitrate,
    /// MPEG-4 / Dolby metadata mode. 0 disables it, which is the default.
    MetadataMode,
    /// Channel ordering: 0 for MPEG order (the default), 1 for WAV order.
    ChannelOrder,
}

impl Param {
    fn value(self) -> sys::AACENC_PARAM {
        match self {
            Param::Afterburner => sys::AACENC_PARAM_AACENC_AFTERBURNER,
            Param::Bandwidth => sys::AACENC_PARAM_AACENC_BANDWIDTH,
            Param::PeakBitrate => sys::AACENC_PARAM_AACENC_PEAK_BITRATE,
            Param::GranuleLength => sys::AACENC_PARAM_AACENC_GRANULE_LENGTH,
            Param::SbrMode => sys::AACENC_PARAM_AACENC_SBR_MODE,
            Param::SbrRatio => sys::AACENC_PARAM_AACENC_SBR_RATIO,
            Param::SignalingMode => sys::AACENC_PARAM_AACENC_SIGNALING_MODE,
            Param::HeaderPeriod => sys::AACENC_PARAM_AACENC_HEADER_PERIOD,
            Param::TpSubframes => sys::AACENC_PARAM_AACENC_TPSUBFRAMES,
            Param::AudioMuxVersion => sys::AACENC_PARAM_AACENC_AUDIOMUXVER,
            Param::Protection => sys::AACENC_PARAM_AACENC_PROTECTION,
            Param::AncillaryBitrate => sys::AACENC_PARAM_AACENC_ANCILLARY_BITRATE,
            Param::MetadataMode => sys::AACENC_PARAM_AACENC_METADATA_MODE,
            Param::ChannelOrder => sys::AACENC_PARAM_AACENC_CHANNELORDER,
        }
    }
}

/// FORK: `Default` and `Clone`, so a caller can set two fields and take the rest — upstream
/// required writing out all five every time, which is how a sample rate ends up copied from
/// an example and left there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderParams {
    pub bit_rate: BitRate,
    pub sample_rate: u32,
    pub transport: Transport,
    pub channels: ChannelMode,
    pub audio_object_type: AudioObjectType,
}

impl Default for EncoderParams {
    /// Plain AAC-LC in ADTS at 48 kHz stereo, 128 kbps — the configuration that is right
    /// when nothing about the application says otherwise.
    fn default() -> Self {
        EncoderParams {
            bit_rate: BitRate::Cbr(128_000),
            sample_rate: 48_000,
            transport: Transport::Adts,
            channels: ChannelMode::Stereo,
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        }
    }
}

/// An AAC encoder.
///
/// `Send` but deliberately **not** `Sync`: `aacEncEncode` rewrites the encoder's internal
/// state, so an encoder may be moved to another thread but never shared with one. Sharing
/// does not compile:
///
/// ```compile_fail
/// use fdk_aac::enc::{Encoder, EncoderParams};
///
/// let encoder = Encoder::new(EncoderParams::default()).unwrap();
/// std::thread::scope(|s| {
///     // error[E0277]: `Encoder` cannot be shared between threads safely
///     s.spawn(|| { let _ = &encoder; });
///     s.spawn(|| { let _ = &encoder; });
/// });
/// ```
///
/// That block is compiled by `cargo test` and the suite fails if it ever *succeeds*, which
/// is the only way to assert a negative trait bound. Upstream 0.8.0 had
/// `unsafe impl Sync for EncoderHandle` and `fn encode(&self, …)`, so the equivalent
/// program compiled and raced; restoring either one makes this doc test fail.
///
/// Moving an encoder is fine and useful:
///
/// ```no_run
/// # use fdk_aac::enc::{Encoder, EncoderParams};
/// let mut encoder = Encoder::new(EncoderParams::default()).unwrap();
/// std::thread::spawn(move || {
///     let info = encoder.info().unwrap();
///     let pcm = vec![0i16; info.frameLength as usize * 2];
///     let mut out = vec![0u8; info.maxOutBufBytes as usize];
///     encoder.encode(&pcm, &mut out).unwrap();
/// });
/// ```
pub struct Encoder {
    handle: EncoderHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeInfo {
    pub input_consumed: usize,
    pub output_size: usize,
}

impl Encoder {
    pub fn new(params: EncoderParams) -> Result<Self, EncoderError> {
        // FORK: the channel count comes from the requested mode. Upstream passed a literal
        // 2 with the comment "hardcode stereo", which over-allocates for the mono case that
        // most streaming applications actually use.
        let handle = EncoderHandle::alloc(0, params.channels.count())?;

        unsafe {
            let set = |param: sys::AACENC_PARAM, value: u32| {
                check(sys::aacEncoder_SetParam(handle.ptr, param, value))
            };

            set(sys::AACENC_PARAM_AACENC_AOT, params.audio_object_type.value() as u32)?;

            // CBR carries a rate; the VBR modes explicitly ignore one, so it is not sent.
            if let BitRate::Cbr(bitrate) = params.bit_rate {
                set(sys::AACENC_PARAM_AACENC_BITRATE, bitrate)?;
            }
            set(sys::AACENC_PARAM_AACENC_BITRATEMODE, params.bit_rate.mode())?;

            set(sys::AACENC_PARAM_AACENC_SAMPLERATE, params.sample_rate)?;
            set(sys::AACENC_PARAM_AACENC_TRANSMUX, params.transport.transmux())?;

            // FORK: `AACENC_SBR_MODE` is deliberately NOT set here.
            //
            // Upstream set it to 0 unconditionally, with the comment "hardcode SBR off for
            // now". `aacenc_lib.h` documents that parameter as being for the ELD object
            // type only, defaulting to -1 (auto) — and 0 does not mean "leave it alone", it
            // means *disable SBR*. The effect was that `Mpeg4HeAac` and `Mpeg4HeAacV2`
            // silently produced plain AAC-LC: the AOT was signalled, the SBR payload was
            // not there, and every file came out sounding like the bitrate it was actually
            // given. Leaving the parameter alone lets the AOT decide, which is how fdk-aac
            // is meant to be driven. `Param::SbrMode` is there for ELD.
            set(sys::AACENC_PARAM_AACENC_CHANNELMODE, params.channels.value())?;

            // The documented initialisation call: everything set, nothing to encode yet.
            check(sys::aacEncEncode(
                handle.ptr,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
            ))?;
        }

        Ok(Encoder { handle })
    }

    /// The encoder's own account of itself: frame length, delay, maximum output size, and
    /// the AudioSpecificConfig a container needs.
    ///
    /// `frameLength` is how many samples per channel one [`encode`](Self::encode) call
    /// wants; `nDelay` is the encoder delay a player must skip for the output to line up
    /// with the input; `confBuf[..confSize]` is the AudioSpecificConfig, which is what goes
    /// in an `esds` box or an `OpusHead`-equivalent for AAC.
    pub fn info(&self) -> Result<InfoStruct, EncoderError> {
        let mut info = MaybeUninit::uninit();
        check(unsafe { sys::aacEncInfo(self.handle.ptr, info.as_mut_ptr()) })?;
        Ok(unsafe { info.assume_init() })
    }

    /// Set one encoder parameter. Must be called before the first [`encode`](Self::encode);
    /// fdk-aac reconfigures on the next frame boundary and some parameters cannot change
    /// once encoding has started.
    ///
    /// FORK: new. See [`Param`].
    pub fn set_param(&mut self, param: Param, value: u32) -> Result<(), EncoderError> {
        check(unsafe { sys::aacEncoder_SetParam(self.handle.ptr, param.value(), value) })
    }

    /// Read one encoder parameter back.
    ///
    /// **Reads the *applied* configuration, not the requested one.** `aacEncoder_SetParam`
    /// stores into a pending `settings` struct and raises an init flag; fdk-aac copies that
    /// into the live config when it reconfigures, which happens on the next
    /// [`encode`](Self::encode). So `set_param` immediately followed by `get_param` returns
    /// the *old* value, and that is fdk-aac working as designed rather than a failure —
    /// `aacenc_lib.cpp` reads `hAacEncoder->aacConfig` here and writes
    /// `settings->user…` there. Encode one frame in between.
    ///
    /// Some parameters are also normalised on the way in: the encoder clamps `Bandwidth` to
    /// what the bitrate and sample rate allow, and reports `SbrRatio` as 0 when SBR is not
    /// active, so those read back as what the encoder decided rather than as what was
    /// asked.
    ///
    /// FORK: new. `aacEncoder_GetParam` has no error channel, so an unsupported parameter
    /// reads as 0 rather than failing.
    pub fn get_param(&self, param: Param) -> u32 {
        unsafe { sys::aacEncoder_GetParam(self.handle.ptr, param.value()) }
    }

    /// Encode one frame.
    ///
    /// `input` is interleaved 16-bit PCM and should hold exactly `frameLength * channels`
    /// samples — fewer is accepted and zero-padded by the encoder at end of stream, more is
    /// buffered. `output` must be at least `maxOutBufBytes` from [`info`](Self::info).
    ///
    /// FORK: `&mut self`. See the note on `EncoderHandle` — `aacEncEncode` rewrites encoder
    /// state, so a shared reference was never sound.
    pub fn encode(&mut self, input: &[i16], output: &mut [u8]) -> Result<EncodeInfo, EncoderError> {
        let input_len = cmp::min(i32::MAX as usize, input.len()) as i32;

        let mut input_buf = input.as_ptr() as *mut i16;
        let mut input_buf_ident: c_int = sys::AACENC_BufferIdentifier_IN_AUDIO_DATA as c_int;
        let mut input_buf_size: c_int = input_len;
        let mut input_buf_el_size: c_int = mem::size_of::<i16>() as c_int;
        let input_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut input_buf as *mut _ as *mut *mut c_void,
            bufferIdentifiers: &mut input_buf_ident as *mut c_int,
            bufSizes: &mut input_buf_size as *mut c_int,
            bufElSizes: &mut input_buf_el_size as *mut c_int,
        };

        let mut output_buf = output.as_mut_ptr();
        let mut output_buf_ident: c_int = sys::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as c_int;
        let mut output_buf_size: c_int = cmp::min(i32::MAX as usize, output.len()) as c_int;
        let mut output_buf_el_size: c_int = mem::size_of::<u8>() as c_int;
        let output_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut output_buf as *mut _ as *mut *mut c_void,
            bufferIdentifiers: &mut output_buf_ident as *mut _,
            bufSizes: &mut output_buf_size as *mut _,
            bufElSizes: &mut output_buf_el_size as *mut _,
        };

        let in_args = sys::AACENC_InArgs { numInSamples: input_len, numAncBytes: 0 };
        let mut out_args = unsafe { mem::zeroed() };

        check(unsafe {
            sys::aacEncEncode(self.handle.ptr, &input_desc, &output_desc, &in_args, &mut out_args)
        })?;

        Ok(EncodeInfo {
            output_size: out_args.numOutBytes as usize,
            input_consumed: out_args.numInSamples as usize,
        })
    }

    /// Drain whatever is still inside the encoder at end of stream.
    ///
    /// fdk-aac signals "nothing left" by returning `AACENC_ENCODE_EOF`, which is not a
    /// fault; this reports it as `Ok(None)` so that the drain loop is a `while let` rather
    /// than an error comparison at every call site.
    ///
    /// FORK: new. Upstream offered no way to flush at all, so the last frames of audio
    /// held in the encoder's lookahead were simply lost — audible as a truncated tail on
    /// every file it produced.
    pub fn flush(&mut self, output: &mut [u8]) -> Result<Option<EncodeInfo>, EncoderError> {
        // `numInSamples: -1` is fdk-aac's documented end-of-stream signal. An empty input
        // slice with a length of 0 would instead mean "no samples this call", which the
        // encoder is entitled to answer by doing nothing, forever.
        let mut input_buf: *mut i16 = ptr::null_mut();
        let mut input_buf_ident: c_int = sys::AACENC_BufferIdentifier_IN_AUDIO_DATA as c_int;
        let mut input_buf_size: c_int = 0;
        let mut input_buf_el_size: c_int = mem::size_of::<i16>() as c_int;
        let input_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut input_buf as *mut _ as *mut *mut c_void,
            bufferIdentifiers: &mut input_buf_ident as *mut c_int,
            bufSizes: &mut input_buf_size as *mut c_int,
            bufElSizes: &mut input_buf_el_size as *mut c_int,
        };

        let mut output_buf = output.as_mut_ptr();
        let mut output_buf_ident: c_int = sys::AACENC_BufferIdentifier_OUT_BITSTREAM_DATA as c_int;
        let mut output_buf_size: c_int = cmp::min(i32::MAX as usize, output.len()) as c_int;
        let mut output_buf_el_size: c_int = mem::size_of::<u8>() as c_int;
        let output_desc = sys::AACENC_BufDesc {
            numBufs: 1,
            bufs: &mut output_buf as *mut _ as *mut *mut c_void,
            bufferIdentifiers: &mut output_buf_ident as *mut _,
            bufSizes: &mut output_buf_size as *mut _,
            bufElSizes: &mut output_buf_el_size as *mut _,
        };

        let in_args = sys::AACENC_InArgs { numInSamples: -1, numAncBytes: 0 };
        let mut out_args = unsafe { mem::zeroed() };

        match check(unsafe {
            sys::aacEncEncode(self.handle.ptr, &input_desc, &output_desc, &in_args, &mut out_args)
        }) {
            Ok(()) => Ok(Some(EncodeInfo {
                output_size: out_args.numOutBytes as usize,
                input_consumed: out_args.numInSamples as usize,
            })),
            Err(e) if e.is_eof() => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Debug for Encoder {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Encoder {{ handle: {:?} }}", self.handle.ptr)
    }
}
