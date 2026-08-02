//! The AAC decoder.
//!
//! Descended from `fdk-aac` 0.8.0 by Hailey Somerville (MIT). The error type and its
//! message table are upstream's verbatim — they are a faithful transcription of
//! `aacdecoder_lib.h` and there is nothing to improve about them. What this fork adds is
//! marked `FORK:`.

use std::fmt::{self, Debug, Display};
use std::os::raw::{c_int, c_uint};

use fdk_aac_prebuilt_sys as sys;

pub use sys::CStreamInfo as StreamInfo;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DecoderError(sys::AAC_DECODER_ERROR);

impl DecoderError {
    pub const OUT_OF_MEMORY: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_OUT_OF_MEMORY);
    pub const UNKNOWN: DecoderError = DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNKNOWN);
    pub const TRANSPORT_SYNC_ERROR: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_TRANSPORT_SYNC_ERROR);
    pub const NOT_ENOUGH_BITS: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_NOT_ENOUGH_BITS);
    pub const INVALID_HANDLE: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_INVALID_HANDLE);
    pub const UNSUPPORTED_AOT: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_AOT);
    pub const UNSUPPORTED_FORMAT: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_FORMAT);
    pub const UNSUPPORTED_ER_FORMAT: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_ER_FORMAT);
    pub const UNSUPPORTED_EPCONFIG: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_EPCONFIG);
    pub const UNSUPPORTED_MULTILAYER: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_MULTILAYER);
    pub const UNSUPPORTED_CHANNELCONFIG: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_CHANNELCONFIG);
    pub const UNSUPPORTED_SAMPLINGRATE: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_SAMPLINGRATE);
    pub const INVALID_SBR_CONFIG: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_INVALID_SBR_CONFIG);
    pub const SET_PARAM_FAIL: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_SET_PARAM_FAIL);
    pub const NEED_TO_RESTART: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_NEED_TO_RESTART);
    pub const OUTPUT_BUFFER_TOO_SMALL: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_OUTPUT_BUFFER_TOO_SMALL);
    pub const TRANSPORT_ERROR: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_TRANSPORT_ERROR);
    pub const PARSE_ERROR: DecoderError = DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_PARSE_ERROR);
    pub const UNSUPPORTED_EXTENSION_PAYLOAD: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_EXTENSION_PAYLOAD);
    pub const DECODE_FRAME_ERROR: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_DECODE_FRAME_ERROR);
    pub const CRC_ERROR: DecoderError = DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_CRC_ERROR);
    pub const INVALID_CODE_BOOK: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_INVALID_CODE_BOOK);
    pub const UNSUPPORTED_PREDICTION: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_PREDICTION);
    pub const UNSUPPORTED_CCE: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_CCE);
    pub const UNSUPPORTED_LFE: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_LFE);
    pub const UNSUPPORTED_GAIN_CONTROL_DATA: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_GAIN_CONTROL_DATA);
    pub const UNSUPPORTED_SBA: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_SBA);
    pub const TNS_READ_ERROR: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_TNS_READ_ERROR);
    pub const RVLC_ERROR: DecoderError = DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_RVLC_ERROR);
    pub const ANC_DATA_ERROR: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_ANC_DATA_ERROR);
    pub const TOO_SMALL_ANC_BUFFER: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_TOO_SMALL_ANC_BUFFER);
    pub const TOO_MANY_ANC_ELEMENTS: DecoderError =
        DecoderError(sys::AAC_DECODER_ERROR_AAC_DEC_TOO_MANY_ANC_ELEMENTS);

    /// The raw `AAC_DECODER_ERROR` code.
    ///
    /// FORK: new, matching `EncoderError::code`.
    pub fn code(&self) -> sys::AAC_DECODER_ERROR {
        self.0
    }

    /// Whether the output buffer still holds usable audio.
    ///
    /// fdk-aac divides its errors into ranges, and the distinction is operational rather
    /// than cosmetic: a *decode* error means the frame was concealed and playback should
    /// continue, while an *init* error means the stream cannot be decoded at all. Getting
    /// this backwards means either dropping a stream over one corrupt frame or playing
    /// silence forever.
    ///
    /// FORK: new. Upstream exposed the codes but not the ranges they fall in.
    pub fn is_concealed(&self) -> bool {
        self.0 >= sys::AAC_DECODER_ERROR_aac_dec_decode_error_start
            && self.0 <= sys::AAC_DECODER_ERROR_aac_dec_decode_error_end
    }

    pub fn message(&self) -> &'static str {
        match self.0 {
            sys::AAC_DECODER_ERROR_AAC_DEC_OK => {
                "No error occurred. Output buffer is valid and error free."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_OUT_OF_MEMORY => {
                "Heap returned NULL pointer. Output buffer is invalid."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNKNOWN => {
                "Error condition is of unknown reason, or from a another module. Output buffer is invalid."
            }
            sys::AAC_DECODER_ERROR_aac_dec_sync_error_start => {
                "Synchronization errors. Output buffer is invalid."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_TRANSPORT_SYNC_ERROR => {
                "The transport decoder had synchronization problems. Do not exit decoding. Just feed new bitstream data."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_NOT_ENOUGH_BITS => "The input buffer ran out of bits.",
            sys::AAC_DECODER_ERROR_aac_dec_init_error_start => {
                "Initialization errors. Output buffer is invalid."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_INVALID_HANDLE => {
                "The handle passed to the function call was invalid (NULL)."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_AOT => {
                "The AOT found in the configuration is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_FORMAT => {
                "The bitstream format is not supported. "
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_ER_FORMAT => {
                "The error resilience tool format is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_EPCONFIG => {
                "The error protection format is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_MULTILAYER => {
                "More than one layer for AAC scalable is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_CHANNELCONFIG => {
                "The channel configuration (either number or arrangement) is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_SAMPLINGRATE => {
                "The sample rate specified in the configuration is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_INVALID_SBR_CONFIG => {
                "The SBR configuration is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_SET_PARAM_FAIL => {
                "The parameter could not be set. Either the value was out of range or the parameter does  not exist."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_NEED_TO_RESTART => {
                "The decoder needs to be restarted, since the required configuration change cannot be performed."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_OUTPUT_BUFFER_TOO_SMALL => {
                "The provided output buffer is too small."
            }
            sys::AAC_DECODER_ERROR_aac_dec_decode_error_start => {
                "Decode errors. Output buffer is valid but concealed."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_TRANSPORT_ERROR => {
                "The transport decoder encountered an unexpected error."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_PARSE_ERROR => {
                "Error while parsing the bitstream. Most probably it is corrupted, or the system crashed."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_EXTENSION_PAYLOAD => {
                "Error while parsing the extension payload of the bitstream. The extension payload type found is not supported."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_DECODE_FRAME_ERROR => {
                "The parsed bitstream value is out of range. Most probably the bitstream is corrupt, or the system crashed."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_CRC_ERROR => "The embedded CRC did not match.",
            sys::AAC_DECODER_ERROR_AAC_DEC_INVALID_CODE_BOOK => {
                "An invalid codebook was signaled. Most probably the bitstream is corrupt, or the system  crashed."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_PREDICTION => {
                "Predictor found, but not supported in the AAC Low Complexity profile. Most probably the bitstream is corrupt, or has a wrong format."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_CCE => {
                "A CCE element was found which is not supported. Most probably the bitstream is corrupt, or has a wrong format."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_LFE => {
                "A LFE element was found which is not supported. Most probably the bitstream is corrupt, or has a wrong format."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_GAIN_CONTROL_DATA => {
                "Gain control data found but not supported. Most probably the bitstream is corrupt, or has a wrong format."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_UNSUPPORTED_SBA => {
                "SBA found, but currently not supported in the BSAC profile."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_TNS_READ_ERROR => {
                "Error while reading TNS data. Most probably the bitstream is corrupt or the system crashed."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_RVLC_ERROR => {
                "Error while decoding error resilient data."
            }
            sys::AAC_DECODER_ERROR_aac_dec_anc_data_error_start => {
                "Ancillary data errors. Output buffer is valid."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_ANC_DATA_ERROR => {
                "Non severe error concerning the ancillary data handling."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_TOO_SMALL_ANC_BUFFER => {
                "The registered ancillary data buffer is too small to receive the parsed data."
            }
            sys::AAC_DECODER_ERROR_AAC_DEC_TOO_MANY_ANC_ELEMENTS => {
                "More than the allowed number of ancillary data elements should be written to buffer."
            }
            _ => "Unknown error",
        }
    }
}

impl Debug for DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "DecoderError {{ code: {:?}, message: {:?} }}", self.0 as c_int, self.message())
    }
}

impl Display for DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

// FORK: as with EncoderError — upstream implemented neither, so `?` into a boxed error did
// not work.
impl std::error::Error for DecoderError {}

fn check(e: sys::AAC_DECODER_ERROR) -> Result<(), DecoderError> {
    if e == sys::AAC_DECODER_ERROR_AAC_DEC_OK {
        Ok(())
    } else {
        Err(DecoderError(e))
    }
}

/// A decoder parameter, for [`Decoder::set_param`].
///
/// FORK: new. Upstream exposed exactly two settings, both as bespoke methods
/// (`set_min_output_channels`, `set_max_output_channels`), and nothing else — so the output
/// limiter and the concealment strategy, which are the two things a streaming application
/// actually needs to tune, were unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Param {
    /// Minimum number of output channels; the decoder upmixes to reach it.
    MinOutputChannels,
    /// Maximum number of output channels; the decoder downmixes to fit.
    MaxOutputChannels,
    /// What to do with a two-channel element: 0 leaves it alone, 1 and 2 select one
    /// channel for both outputs, 3 mixes them.
    DualChannelOutputMode,
    /// Channel ordering: 0 for WAV order, 1 for MPEG order.
    OutputChannelMapping,
    /// The signal limiter that prevents clipping on downmix. 0 off, 1 on, -1 auto.
    LimiterEnable,
    /// Limiter attack time in milliseconds.
    LimiterAttackTime,
    /// Limiter release time in milliseconds.
    LimiterReleaseTime,
    /// Error concealment: 0 spectral muting, 1 noise substitution, 2 energy interpolation.
    /// Which one sounds least bad depends entirely on the content.
    ConcealMethod,
    /// Dynamic range control boost factor, 0..=127.
    DrcBoostFactor,
    /// Dynamic range control attenuation factor, 0..=127.
    DrcAttenuationFactor,
    /// DRC reference level in units of -0.25 dB.
    DrcReferenceLevel,
    /// DRC heavy compression on or off.
    DrcHeavyCompression,
    /// Discard everything buffered in the transport layer — what to call after a seek.
    ClearTransportBuffer,
}

impl Param {
    fn value(self) -> sys::AACDEC_PARAM {
        match self {
            Param::MinOutputChannels => sys::AACDEC_PARAM_AAC_PCM_MIN_OUTPUT_CHANNELS,
            Param::MaxOutputChannels => sys::AACDEC_PARAM_AAC_PCM_MAX_OUTPUT_CHANNELS,
            Param::DualChannelOutputMode => sys::AACDEC_PARAM_AAC_PCM_DUAL_CHANNEL_OUTPUT_MODE,
            Param::OutputChannelMapping => sys::AACDEC_PARAM_AAC_PCM_OUTPUT_CHANNEL_MAPPING,
            Param::LimiterEnable => sys::AACDEC_PARAM_AAC_PCM_LIMITER_ENABLE,
            Param::LimiterAttackTime => sys::AACDEC_PARAM_AAC_PCM_LIMITER_ATTACK_TIME,
            Param::LimiterReleaseTime => sys::AACDEC_PARAM_AAC_PCM_LIMITER_RELEAS_TIME,
            Param::ConcealMethod => sys::AACDEC_PARAM_AAC_CONCEAL_METHOD,
            Param::DrcBoostFactor => sys::AACDEC_PARAM_AAC_DRC_BOOST_FACTOR,
            Param::DrcAttenuationFactor => sys::AACDEC_PARAM_AAC_DRC_ATTENUATION_FACTOR,
            Param::DrcReferenceLevel => sys::AACDEC_PARAM_AAC_DRC_REFERENCE_LEVEL,
            Param::DrcHeavyCompression => sys::AACDEC_PARAM_AAC_DRC_HEAVY_COMPRESSION,
            Param::ClearTransportBuffer => sys::AACDEC_PARAM_AAC_TPDEC_CLEAR_BUFFER,
        }
    }
}

/// The bitstream format the decoder expects.
///
/// FORK: upstream had `Raw` and `Adts` only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Bare access units, with the AudioSpecificConfig supplied out-of-band through
    /// [`Decoder::config_raw`].
    Raw,
    /// ADTS, which is what a `.aac` file holds.
    Adts,
    /// LOAS/LATM with AudioMuxElement framing.
    Loas,
    /// LATM with a single MuxConfigPresent layer.
    Latm,
    /// ADIF: one header, then frames to the end of the stream.
    Adif,
}

impl Transport {
    fn transport_type(self) -> sys::TRANSPORT_TYPE {
        match self {
            Transport::Raw => sys::TRANSPORT_TYPE_TT_MP4_RAW,
            Transport::Adts => sys::TRANSPORT_TYPE_TT_MP4_ADTS,
            Transport::Loas => sys::TRANSPORT_TYPE_TT_MP4_LOAS,
            Transport::Latm => sys::TRANSPORT_TYPE_TT_MP4_LATM_MCP1,
            Transport::Adif => sys::TRANSPORT_TYPE_TT_MP4_ADIF,
        }
    }
}

#[derive(Debug)]
pub struct Decoder {
    handle: sys::HANDLE_AACDECODER,
}

// `Sync` is sound here, unlike on the encoder: every method that touches decoder state
// takes `&mut self`, and the two that take `&self` — `stream_info` and
// `decoded_frame_size` — only read a struct the decoder owns.
unsafe impl Send for Decoder {}
unsafe impl Sync for Decoder {}

impl Decoder {
    /// FORK: returns `Result`. `aacDecoder_Open` returns NULL on allocation failure, and
    /// upstream stored that NULL in the struct and returned `Self` — so the failure
    /// surfaced later as `AAC_DEC_INVALID_HANDLE` from an unrelated call, or as a null
    /// dereference in `stream_info`, which is safe code returning a reference to address
    /// zero.
    pub fn new(transport: Transport) -> Result<Self, DecoderError> {
        let handle = unsafe { sys::aacDecoder_Open(transport.transport_type(), 1) };
        if handle.is_null() {
            return Err(DecoderError::OUT_OF_MEMORY);
        }
        Ok(Decoder { handle })
    }

    /// Supply the AudioSpecificConfig for a [`Transport::Raw`] stream — the bytes an
    /// encoder reports in `InfoStruct::confBuf`, or that an MP4 `esds` box carries.
    pub fn config_raw(&mut self, audio_specific_config: &[u8]) -> Result<(), DecoderError> {
        unsafe {
            let mut asc_ptr = audio_specific_config.as_ptr() as *mut u8;
            let asc_len = audio_specific_config.len() as c_uint;
            check(sys::aacDecoder_ConfigRaw(
                self.handle,
                &mut asc_ptr as *mut _,
                &asc_len as *const _,
            ))
        }
    }

    pub fn set_min_output_channels(&mut self, channels: usize) -> Result<(), DecoderError> {
        self.set_param(Param::MinOutputChannels, channels as i32)
    }

    pub fn set_max_output_channels(&mut self, channels: usize) -> Result<(), DecoderError> {
        self.set_param(Param::MaxOutputChannels, channels as i32)
    }

    /// Set one decoder parameter. See [`Param`].
    ///
    /// FORK: new; the two methods above are now thin wrappers over it, so they cannot drift
    /// from it.
    pub fn set_param(&mut self, param: Param, value: i32) -> Result<(), DecoderError> {
        unsafe { check(sys::aacDecoder_SetParam(self.handle, param.value(), value)) }
    }

    /// Hand the decoder some bitstream. Returns how many bytes it took, which is not
    /// necessarily all of them — the rest must be offered again after the next
    /// [`decode_frame`](Self::decode_frame).
    pub fn fill(&mut self, data: &[u8]) -> Result<usize, DecoderError> {
        unsafe {
            // `cast_mut`, and it is sound: fdk-aac takes a `UCHAR **` here purely so it can
            // advance the caller's pointer past what it consumed, and never writes through
            // it. The pointer it advances is the local below, not the caller's slice.
            let mut data_ptr = data.as_ptr().cast_mut();
            let data_len = data.len() as c_uint;
            let mut bytes_valid: c_uint = data_len;

            check(sys::aacDecoder_Fill(
                self.handle,
                &mut data_ptr as *mut _,
                &data_len as *const _,
                &mut bytes_valid as *mut _,
            ))?;

            Ok(data.len() - bytes_valid as usize)
        }
    }

    /// Decode one frame into `pcm`, which must hold at least
    /// [`decoded_frame_size`](Self::decoded_frame_size) samples.
    pub fn decode_frame(&mut self, pcm: &mut [i16]) -> Result<(), DecoderError> {
        unsafe {
            check(sys::aacDecoder_DecodeFrame(self.handle, pcm.as_mut_ptr(), pcm.len() as c_int, 0))
        }
    }

    /// Interleaved samples in one decoded frame: channels × frame size.
    ///
    /// Zero before the first frame has been decoded, because the decoder does not know the
    /// stream's shape until it has parsed a header.
    pub fn decoded_frame_size(&self) -> usize {
        let stream_info = self.stream_info();
        stream_info.numChannels as usize * stream_info.frameSize as usize
    }

    pub fn stream_info(&self) -> &StreamInfo {
        unsafe { &*sys::aacDecoder_GetStreamInfo(self.handle) }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            sys::aacDecoder_Close(self.handle);
        }
    }
}
