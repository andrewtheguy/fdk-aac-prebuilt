//! Is the linked archive the fdk-aac this crate was generated against?
//!
//! Everything else in this test suite assumes the answer is yes. On a machine with a system
//! libfdk-aac installed, or with a stale `$CARGO_HOME/fdk-aac-prebuilt/` cache, or after a
//! release that shipped the wrong archive, it can be no — and every symptom of that is
//! subtle: the API still links, the calls still succeed, and the audio is still audio.
//!
//! libopus-prebuilt answers this with `opus_get_version_string()`. fdk-aac has no
//! equivalent, so the substitute is `version.rs`, generated from the pinned sources by
//! gen-version.sh, compared against what the linked library reports about itself.

mod common;

use fdk_aac::enc::*;
use fdk_aac::version;
use fdk_aac_prebuilt_sys as sys;

/// Ask a `LIB_INFO` table for one module's version and title.
///
/// `aacEncGetLibInfo` fills an array with one entry per module — the encoder, the SBR
/// encoder, the transport writer, and so on — terminated by an entry whose `module_id` is
/// `FDK_NONE`. The entry we want is found by id, not by index, because the order is not
/// documented and has changed between fdk-aac releases.
fn module_info(info: &[sys::LIB_INFO], module: sys::FDK_MODULE_ID) -> Option<(u32, String)> {
    info.iter().find(|entry| entry.module_id == module).map(|entry| {
        let title = if entry.title.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(entry.title) }.to_string_lossy().into_owned()
        };
        (entry.version as u32, title)
    })
}

/// `LIB_INFO` must be zeroed before `aacEncGetLibInfo` sees it — fdk-aac scans for the
/// first free slot and writes there, so uninitialised memory makes it write past the end.
fn lib_info_table() -> Vec<sys::LIB_INFO> {
    vec![unsafe { std::mem::zeroed() }; 32]
}

#[test]
fn links_the_pinned_encoder() {
    let mut info = lib_info_table();
    let err = unsafe { sys::aacEncGetLibInfo(info.as_mut_ptr()) };
    assert_eq!(err, sys::AACENC_ERROR_AACENC_OK, "aacEncGetLibInfo failed");

    let (packed, title) =
        module_info(&info, sys::FDK_MODULE_ID_FDK_AACENC).expect("no AAC encoder module reported");
    println!("linked encoder: {title} {packed:#x}");

    assert_eq!(title, "AAC Encoder", "the encoder module is not fdk-aac's");
    assert_eq!(
        packed,
        version::packed(version::ENCODER_LIB_VERSION),
        "the linked encoder is version {:#x}, but this crate was generated against \
         fdk-aac {} whose encoder is {:?} ({:#x}). Something other than the pinned archive \
         satisfied the link — a system libfdk-aac, or a stale prebuilt cache.",
        packed,
        version::FDK_AAC_VERSION,
        version::ENCODER_LIB_VERSION,
        version::packed(version::ENCODER_LIB_VERSION),
    );
}

#[test]
fn links_the_pinned_decoder() {
    let mut info = lib_info_table();
    let err = unsafe { sys::aacDecoder_GetLibInfo(info.as_mut_ptr()) };
    assert_eq!(err, 0, "aacDecoder_GetLibInfo failed");

    let (packed, title) =
        module_info(&info, sys::FDK_MODULE_ID_FDK_AACDEC).expect("no AAC decoder module reported");
    println!("linked decoder: {title} {packed:#x}");

    assert_eq!(title, "AAC Decoder Lib", "the decoder module is not fdk-aac's");
    assert_eq!(
        packed,
        version::packed(version::DECODER_LIB_VERSION),
        "the linked decoder is version {packed:#x}, but this crate was generated against \
         fdk-aac {} whose decoder is {:?}.",
        version::FDK_AAC_VERSION,
        version::DECODER_LIB_VERSION,
    );
}

/// `save_audio_stream`'s configuration, exactly: 16 kHz mono AAC-LC in ADTS at 32 kbps.
///
/// The four `InfoStruct` fields checked here are the ones that project persists per segment
/// and needs for gapless playback. A wrong `frameLength` means every segment boundary is in
/// the wrong place; a wrong `nDelay` means every segment starts with a few milliseconds of
/// the previous one.
#[test]
fn encodes_like_a_streaming_consumer() {
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(32_000),
        sample_rate: 16_000,
        transport: Transport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .expect("the 16 kHz mono AAC-LC configuration must be encodable");

    let info = encoder.info().unwrap();
    println!(
        "frameLength={} nDelay={} nDelayCore={} maxOutBufBytes={} confSize={}",
        info.frameLength, info.nDelay, info.nDelayCore, info.maxOutBufBytes, info.confSize
    );

    // 1024 samples is AAC-LC's granule, and it is not a detail: it is the unit every
    // consumer's segmenting arithmetic is written in.
    assert_eq!(info.frameLength, 1024, "AAC-LC's frame is 1024 samples");
    assert!(info.nDelay > 0, "an encoder with no reported delay cannot be aligned against");
    assert!(info.maxOutBufBytes > 0);
    assert_eq!(info.inputChannels, 1, "the encoder was opened for the wrong channel count");

    // The AudioSpecificConfig. Two bytes for plain AAC-LC, and they encode the object type,
    // the sample rate index and the channel configuration — which is exactly what an MP4
    // `esds` box or a raw-transport decoder needs, and what upstream 0.8.0 gave no way to
    // check was populated.
    assert!(info.confSize >= 2, "no AudioSpecificConfig was produced");
    let asc = &info.confBuf[..info.confSize as usize];
    println!("AudioSpecificConfig: {asc:02x?}");
    // 5 bits object type = 2 (AAC-LC), 4 bits frequency index = 8 (16 kHz), 4 bits channel
    // configuration = 1 (mono). That packs to 0b00010_1000_0001_000 = 0x14 0x08.
    assert_eq!(asc[0] >> 3, 2, "AudioSpecificConfig does not say AAC-LC");
    let freq_index = ((asc[0] & 0x07) << 1) | (asc[1] >> 7);
    assert_eq!(freq_index, common::frequency_index(16_000).unwrap());
    assert_eq!((asc[1] >> 3) & 0x0F, 1, "AudioSpecificConfig does not say mono");

    let pcm = common::tone(16_000, info.frameLength as usize).samples;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let encoded = encoder.encode(&pcm, &mut out).unwrap();
    assert_eq!(encoded.input_consumed, pcm.len(), "the encoder did not take the whole frame");

    // The first call returns nothing — the frame is inside the encoder's lookahead — which
    // is itself worth asserting, because a consumer that assumes one frame in means one
    // frame out drops audio at the start of every stream.
    let mut produced = encoded.output_size;
    for _ in 0..8 {
        produced += encoder.encode(&pcm, &mut out).unwrap().output_size;
    }
    assert!(produced > 0, "nine frames in and nothing came out");
}

/// The bitrate really is the bitrate. A CBR encoder that quietly ignored
/// `AACENC_BITRATE` — the failure a transposed parameter constant produces — makes files
/// of the wrong size and nothing else complains.
#[test]
fn cbr_hits_its_target() {
    const RATE: u32 = 48_000;
    const BITRATE: u32 = 64_000;
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(BITRATE),
        sample_rate: RATE,
        transport: Transport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();

    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize;
    // Two seconds, so the bit reservoir has room to average out.
    let signal = common::chord(RATE, RATE as usize * 2);
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut total = 0usize;
    let mut frames = 0usize;

    for chunk in signal.samples.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        total += r.output_size;
        if r.output_size > 0 {
            frames += 1;
        }
    }

    let seconds = frames as f64 * frame as f64 / RATE as f64;
    let measured = total as f64 * 8.0 / seconds;
    println!("asked for {BITRATE} bps, got {measured:.0} bps over {seconds:.2} s");
    // ±12%: ADTS headers cost 7 bytes a frame, and the reservoir means the last frames are
    // not settled. Wide enough never to be flaky, narrow enough that "the parameter was
    // ignored" — which produces fdk-aac's default, a very different number — cannot pass.
    assert!(
        (measured - BITRATE as f64).abs() / (BITRATE as f64) < 0.12,
        "measured {measured:.0} bps against a requested {BITRATE}"
    );
}

/// Every parameter this crate exposes must round-trip through fdk-aac.
///
/// This is the test that catches a wrong `AACENC_PARAM` constant, which is the single most
/// likely defect in a hand-written mapping table: setting the wrong parameter *succeeds*,
/// and reading back the one you named then returns something else.
///
/// The `encode` call in the middle is not padding. `aacEncoder_SetParam` writes to a pending
/// settings struct and raises an init flag; `aacEncoder_GetParam` reads the live config,
/// which fdk-aac only refreshes when it reconfigures on the next frame. Without the encode,
/// every assertion here compares against the value from before the set — see the note on
/// `Encoder::get_param`.
#[test]
fn encoder_params_round_trip() {
    const RATE: u32 = 48_000;
    let mut e = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(96_000),
        sample_rate: RATE,
        transport: Transport::Adts,
        channels: ChannelMode::Stereo,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();

    // Deliberately excluded, all three because fdk-aac *derives* what it reports rather
    // than storing what it was given, so an equality assertion would be pinning its policy
    // rather than this crate's mapping: `Bandwidth` is clamped to what the bitrate and
    // sample rate allow, `SbrRatio` reads 0 whenever SBR is inactive, and `SignalingMode`
    // goes through `getSbrSignalingMode()` and reads -1 on a stream without SBR. The last
    // one is asserted in heaac.rs, where it means something.
    let settings = [
        (Param::Afterburner, 1),
        (Param::PeakBitrate, 128_000),
        (Param::HeaderPeriod, 10),
        (Param::Protection, 1),
        (Param::MetadataMode, 0),
        (Param::ChannelOrder, 1),
    ];

    for (param, value) in settings {
        e.set_param(param, value).unwrap_or_else(|err| panic!("set {param:?} = {value}: {err}"));
    }

    let info = e.info().unwrap();
    let pcm = common::tone(RATE, info.frameLength as usize * 2).samples;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    e.encode(&pcm, &mut out).expect("the encoder must accept the reconfiguration");

    for (param, value) in settings {
        assert_eq!(
            e.get_param(param),
            value,
            "{param:?} was set to {value} and read back as {}",
            e.get_param(param)
        );
    }
}

/// A parameter fdk-aac rejects must come back as an error rather than being swallowed.
/// `Afterburner` is documented as 0 or 1 only.
#[test]
fn an_out_of_range_param_is_rejected() {
    let mut e = Encoder::new(EncoderParams::default()).unwrap();
    match e.set_param(Param::Afterburner, 7) {
        Err(err) => println!("rejected, as it should be: {err}"),
        Ok(()) => panic!("afterburner = 7 was accepted"),
    }
}

/// An invalid configuration must be an error, not a panic and not a silently different
/// encoder. 96 kHz is beyond what AAC-LC accepts.
#[test]
fn an_impossible_configuration_is_an_error() {
    let result = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: 96_001,
        transport: Transport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    });
    match result {
        Err(e) => println!("rejected, as it should be: {e}"),
        Ok(_) => panic!("96001 Hz was accepted as a sample rate"),
    }
}
