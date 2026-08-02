//! The encoder across every configuration this crate exposes, with the results read back
//! out of the bitstream rather than taken on trust.
//!
//! The reason this file is a matrix rather than a couple of cases: fdk-aac takes its
//! configuration as bare integers, and every mapping in `enc.rs` — sample rate, channel
//! mode, transport, bitrate mode, object type — is a chance to send the wrong one. None of
//! those produces an error. The encoder configures itself to whatever it was actually told
//! and returns bytes, and the mistake surfaces months later as a file some decoder somewhere
//! will not open. Parsing the ADTS header back is what turns that into a failing test.

mod common;

use common::{adts_frames, frequency_index, Adts};
use fdk_aac::enc::*;

/// Every sample rate a consumer is likely to use. 7350 and 96000 are left out: the former is
/// only reachable through SBR, and the latter is outside AAC-LC.
const RATES: [u32; 9] = [8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000];

/// Encode a signal through one configuration and return the whole bitstream.
fn encode_all(params: EncoderParams, samples: &[i16]) -> (InfoStruct, Vec<u8>) {
    let mut encoder = Encoder::new(params).expect("configuration must be encodable");
    let info = encoder.info().unwrap();
    let channels = params.channels.count();
    let frame = info.frameLength as usize * channels;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();

    for chunk in samples.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        stream.extend_from_slice(&out[..r.output_size]);
    }
    // Drain, so the tail of the signal is in the stream too — the thing upstream 0.8.0 had
    // no way to do at all.
    while let Some(r) = encoder.flush(&mut out).unwrap() {
        if r.output_size == 0 {
            break;
        }
        stream.extend_from_slice(&out[..r.output_size]);
    }
    (info, stream)
}

/// Interleave a mono signal up to `channels`.
fn interleave(mono: &[i16], channels: usize) -> Vec<i16> {
    if channels == 1 {
        return mono.to_vec();
    }
    mono.iter().flat_map(|&s| std::iter::repeat(s).take(channels)).collect()
}

/// Every rate × every channel mode, in ADTS, with the header checked against what was asked.
///
/// This is the test that would have caught a swapped `AACENC_SAMPLERATE` /
/// `AACENC_CHANNELMODE` constant: both are `0x010x`, adjacent, and the encoder accepts
/// either value in either slot.
#[test]
fn adts_headers_say_what_was_asked_for() {
    for rate in RATES {
        for channels in [ChannelMode::Mono, ChannelMode::Stereo] {
            let mono = common::chord(rate, rate as usize / 2).samples;
            let pcm = interleave(&mono, channels.count());
            let (_, stream) = encode_all(
                EncoderParams {
                    bit_rate: BitRate::Cbr(64_000),
                    sample_rate: rate,
                    transport: Transport::Adts,
                    channels,
                    audio_object_type: AudioObjectType::Mpeg4LowComplexity,
                },
                &pcm,
            );

            let frames = adts_frames(&stream);
            assert!(
                frames.len() > 3,
                "{rate} Hz {channels:?}: only {} parseable ADTS frames in {} bytes — the \
                 frame_length field is wrong, or the transport is not ADTS at all",
                frames.len(),
                stream.len(),
            );

            let expected_index = frequency_index(rate).unwrap();
            for (i, header) in frames.iter().enumerate() {
                assert_eq!(
                    header.sampling_frequency_index,
                    expected_index,
                    "{rate} Hz {channels:?}, frame {i}: header says {} Hz",
                    header.sample_rate().map(|r| r as i64).unwrap_or(-1),
                );
                assert_eq!(
                    header.channel_configuration as usize,
                    channels.count(),
                    "{rate} Hz {channels:?}, frame {i}: header says {} channels",
                    header.channel_configuration,
                );
                // ADTS stores object_type - 1, so AAC-LC (object type 2) is profile 1. Off
                // by one here means every decoder reads the stream as AAC Main.
                assert_eq!(header.profile, 1, "{rate} Hz {channels:?}: not signalled as AAC-LC");
                assert_eq!(header.mpeg_version, 0, "MPEG-4 was requested");
            }

            // The frames must tile the stream exactly — `adts_frames` walks by the length
            // field, so a stream it fully consumed is one whose lengths are all correct.
            let covered: usize = frames.iter().map(|f| f.frame_length).sum();
            assert_eq!(
                covered,
                stream.len(),
                "{rate} Hz {channels:?}: frame lengths cover {covered} of {} bytes",
                stream.len(),
            );
        }
    }
}

/// MPEG-2 object types must be signalled as MPEG-2 in the ADTS header.
///
/// One bit, and the only thing that distinguishes `Mpeg2Aac` from `Mpeg4LowComplexity` in
/// the output — so a mapping that sent the MPEG-4 object type for both would produce
/// byte-identical files and pass every other test here.
#[test]
fn mpeg2_object_types_are_signalled_as_mpeg2() {
    let pcm = common::chord(48_000, 48_000).samples;
    for (aot, expected_version) in
        [(AudioObjectType::Mpeg4LowComplexity, 0), (AudioObjectType::Mpeg2Aac, 1)]
    {
        let (_, stream) = encode_all(
            EncoderParams {
                bit_rate: BitRate::Cbr(64_000),
                sample_rate: 48_000,
                transport: Transport::Adts,
                channels: ChannelMode::Mono,
                audio_object_type: aot,
            },
            &pcm,
        );
        let frames = adts_frames(&stream);
        assert!(!frames.is_empty(), "{aot:?} produced no parseable frames");
        for header in &frames {
            assert_eq!(
                header.mpeg_version,
                expected_version,
                "{aot:?} was signalled as MPEG-{}",
                if header.mpeg_version == 0 { 4 } else { 2 },
            );
        }
    }
}

/// All five VBR modes plus CBR must produce a stream, and the VBR modes must actually vary.
///
/// The second half matters: `BitRate::mode()` maps to `AACENC_BITRATEMODE`, and a mapping
/// that sent 0 for everything would give five identical constant-bitrate streams that pass
/// any test only checking the bytes are non-empty.
#[test]
fn every_bitrate_mode_encodes() {
    let pcm = interleave(&common::sweep(48_000, 48_000 * 2).samples, 2);
    let modes = [
        BitRate::Cbr(128_000),
        BitRate::VbrVeryLow,
        BitRate::VbrLow,
        BitRate::VbrMedium,
        BitRate::VbrHigh,
        BitRate::VbrVeryHigh,
    ];

    let mut sizes = Vec::new();
    for mode in modes {
        let (_, stream) = encode_all(
            EncoderParams {
                bit_rate: mode,
                sample_rate: 48_000,
                transport: Transport::Adts,
                channels: ChannelMode::Stereo,
                audio_object_type: AudioObjectType::Mpeg4LowComplexity,
            },
            &pcm,
        );
        assert!(!adts_frames(&stream).is_empty(), "{mode:?} produced nothing parseable");
        println!("{mode:?}: {} bytes", stream.len());
        sizes.push(stream.len());
    }

    // The five VBR modes are documented as roughly 32/72/112/148/228 kbps for stereo, so
    // they must come out strictly increasing. Equal sizes would mean the mode never reached
    // the encoder.
    let vbr = &sizes[1..];
    for pair in vbr.windows(2) {
        assert!(
            pair[1] > pair[0],
            "the VBR modes did not increase in size: {vbr:?} — is AACENC_BITRATEMODE reaching \
             the encoder?"
        );
    }
}

/// Each transport must produce its own framing, and only ADTS may look like ADTS.
///
/// FORK: LOAS, LATM and ADIF did not exist upstream. The check that a *non*-ADTS transport
/// fails to parse as ADTS is the one that catches a `TRANSMUX` mapping where every variant
/// happens to send 2.
#[test]
fn each_transport_produces_its_own_framing() {
    let pcm = common::chord(48_000, 48_000).samples;
    let base = EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: 48_000,
        transport: Transport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    };

    let mut streams = Vec::new();
    for transport in
        [Transport::Raw, Transport::Adts, Transport::Loas, Transport::Latm, Transport::Adif]
    {
        let (info, stream) = encode_all(EncoderParams { transport, ..base }, &pcm);
        assert!(!stream.is_empty(), "{transport:?} produced no bytes at all");
        println!("{transport:?}: {} bytes, confSize {}", stream.len(), info.confSize);

        match transport {
            Transport::Adts => {
                let frames = adts_frames(&stream);
                assert!(frames.len() > 3, "ADTS did not produce parseable ADTS");
            }
            _ => {
                // Not "produces no valid ADTS header ever" — a LATM stream can contain the
                // byte pattern by chance — but "does not tile the whole stream as ADTS",
                // which is what a real ADTS stream does and a mislabelled one does not.
                let frames = adts_frames(&stream);
                let covered: usize = frames.iter().map(|f| f.frame_length).sum();
                assert_ne!(
                    covered,
                    stream.len(),
                    "{transport:?} produced a stream that parses cleanly as ADTS — the \
                     TRANSMUX mapping is sending the wrong transport type"
                );
            }
        }
        streams.push((transport, stream));
    }

    // And each is distinct from the others. Two transports that produced identical bytes
    // would mean two enum variants mapping to the same constant.
    for (i, (ta, a)) in streams.iter().enumerate() {
        for (tb, b) in &streams[i + 1..] {
            assert_ne!(a, b, "{ta:?} and {tb:?} produced identical bitstreams");
        }
    }
}

/// Raw transport must carry its configuration out-of-band, and that config must be what a
/// decoder needs.
///
/// This is the pairing that matters for MP4: `Transport::Raw` plus `confBuf` is what goes in
/// an `esds` box, and a `confSize` of zero means a file no player can open.
#[test]
fn raw_transport_reports_an_audio_specific_config() {
    let pcm = common::chord(44_100, 44_100).samples;
    let (info, stream) = encode_all(
        EncoderParams {
            bit_rate: BitRate::Cbr(96_000),
            sample_rate: 44_100,
            transport: Transport::Raw,
            channels: ChannelMode::Mono,
            audio_object_type: AudioObjectType::Mpeg4LowComplexity,
        },
        &pcm,
    );

    assert!(info.confSize >= 2, "raw transport with no AudioSpecificConfig is unusable");
    let asc = &info.confBuf[..info.confSize as usize];
    assert_eq!(asc[0] >> 3, 2, "AudioSpecificConfig does not say AAC-LC");
    let freq_index = ((asc[0] & 0x07) << 1) | (asc[1] >> 7);
    assert_eq!(freq_index, frequency_index(44_100).unwrap(), "wrong rate in the config");

    // No ADTS syncword anywhere near the start: raw means raw.
    assert!(
        Adts::parse(&stream).is_none(),
        "the raw stream begins with an ADTS header — the transport did not take"
    );
}

/// The flush loop must terminate and must produce the tail.
///
/// FORK: `flush` is new. Upstream had no way to drain the encoder, so the last ~2048 samples
/// of every stream it produced were simply lost — and because that is a fraction of a second
/// at the very end, it is the kind of bug that ships.
#[test]
fn flush_drains_the_encoder() {
    let rate = 48_000;
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: rate,
        transport: Transport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();
    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];

    let pcm = common::tone(rate, frame * 10).samples;
    let mut during = 0usize;
    for chunk in pcm.chunks(frame) {
        during += encoder.encode(chunk, &mut out).unwrap().output_size;
    }

    let mut after = 0usize;
    let mut calls = 0;
    while let Some(r) = encoder.flush(&mut out).unwrap() {
        after += r.output_size;
        calls += 1;
        assert!(calls < 64, "flush did not reach end of file after {calls} calls");
    }

    println!("{during} bytes while encoding, {after} more from the flush over {calls} calls");
    assert!(after > 0, "flush produced nothing — the encoder's lookahead was discarded");
    // The delay is ~2048 samples, so a flush should be worth roughly two frames.
    assert!(calls >= 1);
}
