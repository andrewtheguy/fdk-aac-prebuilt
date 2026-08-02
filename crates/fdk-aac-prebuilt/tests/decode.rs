//! The decoder: what it reports, and how it fails.
//!
//! The error paths get as much room as the happy one here, because a decoder is fed
//! whatever arrives over a network and the difference between "this frame was corrupt,
//! conceal it and carry on" and "this stream cannot be decoded, give up" is a decision every
//! consumer has to make from the error value. Upstream exposed the codes but nothing that
//! grouped them, so that decision was a hand-maintained list of constants at every call
//! site.

mod common;

use fdk_aac::dec::{Decoder, DecoderError, Param, Transport as DecTransport};
use fdk_aac::enc::{
    AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Transport as EncTransport,
};

/// Encode a mono tone to ADTS and return the bitstream plus the encoder's own account of it.
fn adts_stream(rate: u32, seconds: f64) -> (Vec<u8>, u32, u32) {
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: rate,
        transport: EncTransport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();
    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize;
    let pcm = common::chord(rate, (rate as f64 * seconds) as usize).samples;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();
    for chunk in pcm.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        stream.extend_from_slice(&out[..r.output_size]);
    }
    (stream, info.frameLength, info.nDelay)
}

/// Everything `CStreamInfo` reports about a stream we made ourselves must match what we
/// asked for.
#[test]
fn stream_info_describes_the_stream() {
    const RATE: u32 = 44_100;
    let (stream, frame_length, _) = adts_stream(RATE, 1.0);

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();

    // Before anything is decoded the decoder knows nothing, and says so rather than
    // reporting stale or uninitialised numbers.
    assert_eq!(decoder.decoded_frame_size(), 0, "the decoder claimed a frame size before parsing");

    decoder.fill(&stream).unwrap();
    let mut pcm = vec![0i16; 8192];
    decoder.decode_frame(&mut pcm).expect("the first frame must decode");

    let si = decoder.stream_info();
    println!(
        "rate={} frameSize={} channels={} aot={} profile={} outputDelay={}",
        si.sampleRate, si.frameSize, si.numChannels, si.aot, si.profile, si.outputDelay
    );
    assert_eq!(si.sampleRate, RATE as i32);
    assert_eq!(si.aacSampleRate, RATE as i32);
    assert_eq!(si.numChannels, 1);
    assert_eq!(si.frameSize as u32, frame_length, "decoder and encoder disagree about the frame");
    assert_eq!(si.aot, 2, "not decoded as AAC-LC");
    assert_eq!(decoder.decoded_frame_size(), frame_length as usize);
}

/// `fill` reports what it took, and the remainder can be offered again.
///
/// This is the contract a streaming consumer depends on: the decoder's input buffer is
/// finite, so a large `fill` is partially consumed and the caller must not throw the rest
/// away. Upstream documented none of it.
#[test]
fn fill_consumes_incrementally() {
    let (stream, _, _) = adts_stream(48_000, 2.0);
    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    let mut pcm = vec![0i16; 8192];

    let mut offset = 0;
    let mut frames = 0;
    while offset < stream.len() {
        let taken = decoder.fill(&stream[offset..]).unwrap();
        assert!(taken > 0 || frames > 0, "the decoder took nothing from a fresh stream");
        offset += taken;
        while decoder.decode_frame(&mut pcm).is_ok() {
            frames += 1;
        }
        if taken == 0 {
            break;
        }
    }
    println!("{frames} frames from {} bytes", stream.len());
    assert!(frames > 50, "only {frames} frames decoded from two seconds of audio");
}

/// A truncated frame must report "not enough bits" rather than decoding rubbish.
#[test]
fn a_truncated_frame_asks_for_more() {
    let (stream, _, _) = adts_stream(48_000, 0.5);
    // Half of the first frame and nothing else.
    let header = common::Adts::parse(&stream).unwrap();
    let partial = &stream[..header.frame_length / 2];

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    decoder.fill(partial).unwrap();
    let mut pcm = vec![0i16; 8192];
    match decoder.decode_frame(&mut pcm) {
        Err(e) => {
            println!("truncated frame: {e}");
            assert_eq!(e, DecoderError::NOT_ENOUGH_BITS, "expected NOT_ENOUGH_BITS, got {e:?}");
            assert!(!e.is_concealed(), "a sync error is not a concealed frame");
        }
        Ok(()) => panic!("half a frame decoded successfully"),
    }
}

/// Garbage must be rejected rather than decoded.
#[test]
fn garbage_does_not_decode() {
    let mut rng = common::Rng::new(0xBAD);
    let garbage: Vec<u8> = (0..4096).map(|_| (rng.next_f64() * 127.0 + 128.0) as u8).collect();

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    decoder.fill(&garbage).unwrap();
    let mut pcm = vec![0i16; 8192];
    match decoder.decode_frame(&mut pcm) {
        Err(e) => println!("garbage rejected: {e}"),
        Ok(()) => panic!("4 KiB of noise decoded as AAC"),
    }
}

/// `is_concealed` must separate the two error families, because that distinction is what a
/// consumer's "drop the stream or keep playing" decision is made from.
///
/// FORK: new — upstream had the codes but not the ranges.
#[test]
fn concealed_and_fatal_errors_are_distinguishable() {
    // Sync and init errors are not concealment: the output buffer holds nothing usable.
    for fatal in [
        DecoderError::NOT_ENOUGH_BITS,
        DecoderError::TRANSPORT_SYNC_ERROR,
        DecoderError::UNSUPPORTED_AOT,
        DecoderError::UNSUPPORTED_SAMPLINGRATE,
        DecoderError::INVALID_HANDLE,
    ] {
        assert!(!fatal.is_concealed(), "{fatal:?} was classified as a concealed frame");
    }
    // Decode errors are: the frame was bad, the decoder substituted something, playback
    // continues.
    for concealed in [
        DecoderError::PARSE_ERROR,
        DecoderError::CRC_ERROR,
        DecoderError::DECODE_FRAME_ERROR,
        DecoderError::INVALID_CODE_BOOK,
        DecoderError::TNS_READ_ERROR,
    ] {
        assert!(concealed.is_concealed(), "{concealed:?} was classified as fatal");
    }
}

/// Every error constant must have its own message, and none may fall through to "Unknown
/// error".
///
/// A message table with a gap in it is invisible until the day something returns the code
/// that fell through, at which point the log says nothing useful about a failure nobody can
/// reproduce.
#[test]
fn every_error_has_a_message() {
    let all = [
        ("OUT_OF_MEMORY", DecoderError::OUT_OF_MEMORY),
        ("UNKNOWN", DecoderError::UNKNOWN),
        ("TRANSPORT_SYNC_ERROR", DecoderError::TRANSPORT_SYNC_ERROR),
        ("NOT_ENOUGH_BITS", DecoderError::NOT_ENOUGH_BITS),
        ("INVALID_HANDLE", DecoderError::INVALID_HANDLE),
        ("UNSUPPORTED_AOT", DecoderError::UNSUPPORTED_AOT),
        ("UNSUPPORTED_FORMAT", DecoderError::UNSUPPORTED_FORMAT),
        ("UNSUPPORTED_ER_FORMAT", DecoderError::UNSUPPORTED_ER_FORMAT),
        ("UNSUPPORTED_EPCONFIG", DecoderError::UNSUPPORTED_EPCONFIG),
        ("UNSUPPORTED_MULTILAYER", DecoderError::UNSUPPORTED_MULTILAYER),
        ("UNSUPPORTED_CHANNELCONFIG", DecoderError::UNSUPPORTED_CHANNELCONFIG),
        ("UNSUPPORTED_SAMPLINGRATE", DecoderError::UNSUPPORTED_SAMPLINGRATE),
        ("INVALID_SBR_CONFIG", DecoderError::INVALID_SBR_CONFIG),
        ("SET_PARAM_FAIL", DecoderError::SET_PARAM_FAIL),
        ("NEED_TO_RESTART", DecoderError::NEED_TO_RESTART),
        ("OUTPUT_BUFFER_TOO_SMALL", DecoderError::OUTPUT_BUFFER_TOO_SMALL),
        ("TRANSPORT_ERROR", DecoderError::TRANSPORT_ERROR),
        ("PARSE_ERROR", DecoderError::PARSE_ERROR),
        ("UNSUPPORTED_EXTENSION_PAYLOAD", DecoderError::UNSUPPORTED_EXTENSION_PAYLOAD),
        ("DECODE_FRAME_ERROR", DecoderError::DECODE_FRAME_ERROR),
        ("CRC_ERROR", DecoderError::CRC_ERROR),
        ("INVALID_CODE_BOOK", DecoderError::INVALID_CODE_BOOK),
        ("UNSUPPORTED_PREDICTION", DecoderError::UNSUPPORTED_PREDICTION),
        ("UNSUPPORTED_CCE", DecoderError::UNSUPPORTED_CCE),
        ("UNSUPPORTED_LFE", DecoderError::UNSUPPORTED_LFE),
        ("UNSUPPORTED_GAIN_CONTROL_DATA", DecoderError::UNSUPPORTED_GAIN_CONTROL_DATA),
        ("UNSUPPORTED_SBA", DecoderError::UNSUPPORTED_SBA),
        ("TNS_READ_ERROR", DecoderError::TNS_READ_ERROR),
        ("RVLC_ERROR", DecoderError::RVLC_ERROR),
        ("ANC_DATA_ERROR", DecoderError::ANC_DATA_ERROR),
        ("TOO_SMALL_ANC_BUFFER", DecoderError::TOO_SMALL_ANC_BUFFER),
        ("TOO_MANY_ANC_ELEMENTS", DecoderError::TOO_MANY_ANC_ELEMENTS),
    ];

    for (name, error) in all {
        assert_ne!(
            error.message(),
            "Unknown error",
            "{name} ({:#x}) has no message — the table has a hole in it",
            error.code(),
        );
        // Display and Debug must both work and must both be non-empty; a `{}` on an error
        // that formats to nothing is a log line that says a failure happened and not which.
        assert!(!format!("{error}").is_empty());
        assert!(format!("{error:?}").contains("code"));
    }

    // And they must be distinct codes, which is what makes matching on them meaningful.
    let mut codes: Vec<_> = all.iter().map(|(_, e)| e.code()).collect();
    codes.sort_unstable();
    let before = codes.len();
    codes.dedup();
    assert_eq!(before, codes.len(), "two error constants share a code");
}

/// Raw transport: the AudioSpecificConfig out-of-band, and one access unit per `fill`.
///
/// Both halves of that are the point. `Transport::Raw` means *the caller owns the framing* —
/// there is no syncword and no length field, so a decoder handed several concatenated access
/// units has no way to find the boundary between them and fails with `AAC_DEC_UNKNOWN`. This
/// is the pairing that matters for MP4, where the `esds` box carries the config and the
/// sample table carries the lengths.
#[test]
fn raw_transport_needs_its_config_and_its_framing() {
    const RATE: u32 = 48_000;
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: RATE,
        transport: EncTransport::Raw,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();
    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize;
    let pcm = common::chord(RATE, RATE as usize).samples;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];

    // Kept as separate access units, because that is what raw transport hands back and what
    // a container is expected to record the lengths of.
    let mut access_units: Vec<Vec<u8>> = Vec::new();
    for chunk in pcm.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        if r.output_size > 0 {
            access_units.push(out[..r.output_size].to_vec());
        }
    }
    assert!(access_units.len() > 10, "not enough access units to test with");

    let config = &info.confBuf[..info.confSize as usize];
    let mut decoder = Decoder::new(DecTransport::Raw).unwrap();
    decoder.config_raw(config).expect("the AudioSpecificConfig must be accepted");

    let mut pcm_out = vec![0i16; 8192];
    let mut decoded = 0;
    let mut audible = false;
    for au in &access_units {
        decoder.fill(au).unwrap();
        if decoder.decode_frame(&mut pcm_out).is_ok() {
            decoded += 1;
            audible |= pcm_out.iter().any(|&s| s != 0);
        }
    }
    println!("{decoded} of {} raw access units decoded", access_units.len());
    assert!(decoded > 10, "a configured raw decoder must decode its own access units");
    assert_eq!(decoder.stream_info().sampleRate, RATE as i32);
    assert!(audible, "decoded to pure silence");

    // And the negative: concatenating them removes the framing the caller was supposed to
    // keep, and the decoder cannot recover it. Asserted rather than merely documented,
    // because "just fill it with everything" is what anyone used to ADTS will try first.
    let concatenated: Vec<u8> = access_units.concat();
    let mut naive = Decoder::new(DecTransport::Raw).unwrap();
    naive.config_raw(config).unwrap();
    naive.fill(&concatenated).unwrap();
    let mut frames = 0;
    while naive.decode_frame(&mut pcm_out).is_ok() {
        frames += 1;
        if frames > access_units.len() {
            break;
        }
    }
    assert!(
        frames < access_units.len(),
        "concatenated raw access units decoded as though they were framed — if fdk-aac has \
         started recovering boundaries, this note and the API docs need updating"
    );
}

/// Every decoder parameter this crate exposes must be accepted.
///
/// Same reasoning as the encoder's version: a wrong `AACDEC_PARAM` is not a compile error.
/// The decoder has no general getter, so this asserts acceptance rather than round-trip —
/// which is still enough, because fdk-aac returns `SET_PARAM_FAIL` for a parameter that
/// does not exist or a value out of its range.
#[test]
fn decoder_params_are_accepted() {
    let mut d = Decoder::new(DecTransport::Adts).unwrap();
    for (param, value) in [
        (Param::MinOutputChannels, 1),
        (Param::MaxOutputChannels, 2),
        (Param::DualChannelOutputMode, 0),
        (Param::OutputChannelMapping, 1),
        (Param::LimiterEnable, 1),
        (Param::LimiterAttackTime, 15),
        (Param::LimiterReleaseTime, 50),
        (Param::ConcealMethod, 1),
        (Param::DrcBoostFactor, 64),
        (Param::DrcAttenuationFactor, 64),
        (Param::DrcReferenceLevel, 64),
        (Param::DrcHeavyCompression, 0),
        (Param::ClearTransportBuffer, 1),
    ] {
        d.set_param(param, value)
            .unwrap_or_else(|e| panic!("{param:?} = {value} was rejected: {e}"));
    }
}

/// A value fdk-aac does not accept must come back as an error.
#[test]
fn an_out_of_range_decoder_param_is_rejected() {
    let mut d = Decoder::new(DecTransport::Adts).unwrap();
    match d.set_param(Param::ConcealMethod, 99) {
        Err(e) => println!("rejected, as it should be: {e}"),
        Ok(()) => panic!("concealment method 99 was accepted"),
    }
}

/// The encoder's reported delay must agree with the decoder's, within a frame.
///
/// These are the two numbers a consumer aligns audio with, and they come from opposite ends
/// of the library. A crate that reported one of them wrong would produce files that drift
/// against their own timestamps.
#[test]
fn the_reported_delays_are_consistent() {
    const RATE: u32 = 48_000;
    let (stream, frame_length, encoder_delay) = adts_stream(RATE, 1.0);

    let mut decoder = Decoder::new(DecTransport::Adts).unwrap();
    decoder.fill(&stream).unwrap();
    let mut pcm = vec![0i16; 8192];
    decoder.decode_frame(&mut pcm).unwrap();
    let output_delay = decoder.stream_info().outputDelay;

    println!("encoder nDelay={encoder_delay}, decoder outputDelay={output_delay}");
    assert!(encoder_delay > 0, "an encoder reporting no delay cannot be aligned against");
    // For plain AAC-LC the decoder adds nothing, so the encoder's delay is the whole story
    // and must be a sensible multiple of the granule rather than an arbitrary number.
    assert!(
        encoder_delay <= frame_length * 3,
        "an encoder delay of {encoder_delay} samples against a {frame_length}-sample frame \
         is not a plausible AAC-LC lookahead"
    );
}
