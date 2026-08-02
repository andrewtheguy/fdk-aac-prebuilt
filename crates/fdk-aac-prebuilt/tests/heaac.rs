//! HE-AAC and HE-AAC v2 must actually contain SBR and PS.
//!
//! **These tests fail against upstream `fdk-aac` 0.8.0**, and that is their point. Upstream
//! set `AACENC_SBR_MODE` to 0 on every encoder it created, with the comment "hardcode SBR
//! off for now". Zero does not mean "leave the default alone" — it means *disable Spectral
//! Band Replication*. So `AudioObjectType::Mpeg4HeAac` signalled HE-AAC in the header,
//! omitted the SBR payload, and produced a plain AAC-LC stream running at whatever bitrate
//! it was given: 32 kbps stereo AAC-LC, which sounds exactly as bad as it sounds.
//!
//! Nothing about that was detectable from the API. `encode` returned `Ok`, the bytes were
//! valid AAC, and a decoder played them. It takes decoding the output and asking the decoder
//! what it found, which is what these tests do.
//!
//! # How SBR is detected here
//!
//! Not by `CStreamInfo::extAot`, or at least not only. fdk-aac forces *implicit* signalling
//! for the MPEG-2 transports — `aacenc_lib.cpp:421-426`, "For MPEG-2 transport types, only
//! implicit signaling is possible" — so an ADTS stream carrying perfectly good SBR reports
//! `extAot` 0 and `aot` 2. The evidence that survives every transport is arithmetic:
//!
//! - the encoder's granule is **2048** samples rather than 1024, because dual-rate SBR runs
//!   the AAC core at half the input rate;
//! - the decoder's output rate is **twice** its core rate (`sampleRate` vs `aacSampleRate`);
//! - with Parametric Stereo the core carries **one** channel and the decoder emits two.
//!
//! `extAot` is asserted as well, on the LOAS path, where explicit signalling is available.

mod common;

use fdk_aac::dec::{Decoder, Transport as DecTransport};
use fdk_aac::enc::*;

/// What the decoder found in a stream we just encoded.
#[derive(Debug)]
struct StreamFacts {
    /// The decoder's *output* rate. With dual-rate SBR this is twice the core rate.
    sample_rate: i32,
    /// The AAC core's rate.
    aac_sample_rate: i32,
    frame_size: i32,
    /// Output channels — with Parametric Stereo this is 2 while the core carries 1.
    channels: i32,
    aac_channels: i32,
    aot: i32,
    /// The *explicitly signalled* extension object type: 5 for SBR, 29 for PS. Zero under
    /// implicit signalling, which is all ADTS can express.
    ext_aot: i32,
    bytes: usize,
}

const AOT_AAC_LC: i32 = 2;
const AOT_SBR: i32 = 5;
const AOT_PS: i32 = 29;

/// Encode a stereo sweep and decode it back.
fn round_trip(
    params: EncoderParams,
    decode_as: DecTransport,
    seconds: f64,
) -> (InfoStruct, StreamFacts) {
    let channels = params.channels.count();
    let mono = common::sweep(params.sample_rate, (params.sample_rate as f64 * seconds) as usize);
    let pcm: Vec<i16> = if channels == 1 {
        mono.samples
    } else {
        mono.samples.iter().flat_map(|&s| std::iter::repeat(s).take(channels)).collect()
    };

    let mut encoder = Encoder::new(params).expect("configuration must be encodable");
    let info = encoder.info().unwrap();
    let frame = info.frameLength as usize * channels;
    let mut out = vec![0u8; info.maxOutBufBytes as usize];
    let mut stream = Vec::new();
    for chunk in pcm.chunks(frame) {
        if chunk.len() < frame {
            break;
        }
        let r = encoder.encode(chunk, &mut out).unwrap();
        stream.extend_from_slice(&out[..r.output_size]);
    }
    assert!(!stream.is_empty(), "the encoder produced nothing");

    let mut decoder = Decoder::new(decode_as).unwrap();
    if decode_as == DecTransport::Raw {
        decoder.config_raw(&info.confBuf[..info.confSize as usize]).unwrap();
    }
    decoder.fill(&stream).unwrap();
    let mut pcm_out = vec![0i16; 16384];
    // Several frames: the decoder does not settle its view of the stream until it has
    // parsed an extension payload, which is not in the very first access unit.
    let mut decoded_any = false;
    for _ in 0..8 {
        if decoder.decode_frame(&mut pcm_out).is_ok() {
            decoded_any = true;
        }
    }
    assert!(decoded_any, "nothing decoded — the stream is not what it claims to be");

    let si = decoder.stream_info();
    let facts = StreamFacts {
        sample_rate: si.sampleRate,
        aac_sample_rate: si.aacSampleRate,
        frame_size: si.frameSize,
        channels: si.numChannels,
        aac_channels: si.aacNumChannels,
        aot: si.aot,
        ext_aot: si.extAot,
        bytes: stream.len(),
    };
    (info, facts)
}

fn params(aot: AudioObjectType, rate: u32, bitrate: u32, transport: Transport) -> EncoderParams {
    EncoderParams {
        bit_rate: BitRate::Cbr(bitrate),
        sample_rate: rate,
        transport,
        channels: ChannelMode::Stereo,
        audio_object_type: aot,
    }
}

/// The control: plain AAC-LC must have no SBR, so that the assertions below are
/// distinguishing HE-AAC from LC rather than passing for everything.
#[test]
fn plain_aac_lc_has_no_sbr() {
    let (info, facts) = round_trip(
        params(AudioObjectType::Mpeg4LowComplexity, 32_000, 64_000, Transport::Adts),
        DecTransport::Adts,
        2.0,
    );
    println!("AAC-LC: {facts:?}");

    assert_eq!(info.frameLength, 1024, "AAC-LC's granule is 1024 samples");
    assert_eq!(facts.aot, AOT_AAC_LC);
    assert_eq!(facts.ext_aot, 0, "plain AAC-LC reported an extension object type");
    assert_eq!(facts.sample_rate, facts.aac_sample_rate, "no SBR means no rate doubling");
    assert_eq!(facts.sample_rate, 32_000);
    assert_eq!(facts.channels, facts.aac_channels, "no PS means no channel synthesis");
}

/// HE-AAC v1 in ADTS: the core must run at half the output rate.
#[test]
fn he_aac_v1_really_has_sbr() {
    let (info, facts) = round_trip(
        params(AudioObjectType::Mpeg4HeAac, 32_000, 48_000, Transport::Adts),
        DecTransport::Adts,
        2.0,
    );
    println!("HE-AAC v1 (ADTS): {facts:?}");

    assert_eq!(
        info.frameLength, 2048,
        "the encoder's granule is {} samples, which is the AAC-LC value — SBR is not \
         active. This is exactly the upstream 0.8.0 bug.",
        info.frameLength,
    );
    assert_eq!(
        facts.sample_rate,
        facts.aac_sample_rate * 2,
        "dual-rate SBR must halve the core rate: core {} Hz, output {} Hz — equal rates \
         mean the SBR payload is not there",
        facts.aac_sample_rate,
        facts.sample_rate,
    );
    assert_eq!(facts.sample_rate, 32_000, "the output rate must be the rate that was asked for");
    assert_eq!(facts.frame_size, 2048, "the decoder's frame must match the encoder's granule");
    // Implicit signalling is all ADTS can carry, so the header says AAC-LC and the decoder
    // finds SBR by inspection. Asserted rather than ignored: if this ever becomes non-zero
    // it means fdk-aac changed how it signals, which consumers embedding these streams in
    // containers would need to know about.
    assert_eq!(facts.ext_aot, 0, "ADTS can only signal SBR implicitly");
}

/// The same stream in LOAS, where explicit signalling *is* available — so the decoder can
/// name the extension rather than infer it.
#[test]
fn he_aac_v1_is_explicitly_signalled_in_loas() {
    let (info, facts) = round_trip(
        params(AudioObjectType::Mpeg4HeAac, 32_000, 48_000, Transport::Loas),
        DecTransport::Loas,
        2.0,
    );
    println!("HE-AAC v1 (LOAS): {facts:?}");

    assert_eq!(info.frameLength, 2048);
    assert_eq!(
        facts.ext_aot, AOT_SBR,
        "LOAS defaults to explicit hierarchical signalling, so the decoder should report \
         extension object type {AOT_SBR} (SBR); it reported {}",
        facts.ext_aot,
    );
    assert_eq!(facts.sample_rate, facts.aac_sample_rate * 2);
}

/// HE-AAC v2: SBR *and* Parametric Stereo. The core carries one channel and the decoder
/// synthesises two — which is the whole trick, and is visible whatever the transport.
#[test]
fn he_aac_v2_really_has_parametric_stereo() {
    let (info, facts) = round_trip(
        params(AudioObjectType::Mpeg4HeAacV2, 44_100, 32_000, Transport::Adts),
        DecTransport::Adts,
        2.0,
    );
    println!("HE-AAC v2 (ADTS): {facts:?}");

    assert_eq!(info.frameLength, 2048, "HE-AAC v2 is dual-rate too");
    assert_eq!(facts.sample_rate, facts.aac_sample_rate * 2, "no SBR in an HE-AAC v2 stream");
    assert_eq!(facts.channels, 2, "PS must decode to two channels");
    assert_eq!(
        facts.aac_channels, 1,
        "with Parametric Stereo the AAC core carries one channel and the decoder \
         synthesises the second; {} core channels means PS did not engage",
        facts.aac_channels,
    );
}

/// And in LOAS, where the extension is named explicitly.
///
/// The extension object type is **`AOT_SBR`, not `AOT_PS`**, and that is correct rather
/// than a shortfall: MPEG signals Parametric Stereo *inside* the SBR extension, as a
/// `ps_present` flag in the payload, so the AudioSpecificConfig's extensionAudioObjectType
/// is SBR for both HE-AAC v1 and v2. What distinguishes v2 is the channel synthesis — one
/// core channel decoded to two — which is what the assertion below actually tests.
///
/// `AOT_PS` (29) is the value used to *request* HE-AAC v2 from the encoder, which is why it
/// is easy to expect it back here and why the mistake is worth a comment rather than a
/// silent fix.
#[test]
fn he_aac_v2_is_explicitly_signalled_in_loas() {
    let (_, facts) = round_trip(
        params(AudioObjectType::Mpeg4HeAacV2, 44_100, 32_000, Transport::Loas),
        DecTransport::Loas,
        2.0,
    );
    println!("HE-AAC v2 (LOAS): {facts:?}");
    assert_eq!(
        facts.ext_aot, AOT_SBR,
        "the decoder found extension object type {} rather than SBR ({AOT_SBR}); PS lives \
         inside the SBR extension and is not signalled as {AOT_PS} here",
        facts.ext_aot,
    );
    assert_eq!(facts.aac_channels, 1, "the PS core must carry one channel");
    assert_eq!(facts.channels, 2, "the decoder must synthesise two");
}

/// HE-AAC really is smaller than LC for the same audio at the same quality target — the
/// reason anyone uses it.
///
/// Compared at a *fixed bitrate* the file sizes would match by definition, so this compares
/// what each needs to represent a 44.1 kHz stereo sweep at its own sensible rate, and
/// checks the HE-AAC v2 stream at 32 kbps is substantially smaller than AAC-LC at 128.
/// A build with SBR silently off would produce an AAC-LC stream at 32 kbps, which is also
/// smaller — so the SBR assertions above are what make this one mean anything, and it is
/// here as the end-to-end sanity check rather than as the proof.
#[test]
fn he_aac_v2_is_much_smaller_than_aac_lc() {
    let (_, lc) = round_trip(
        params(AudioObjectType::Mpeg4LowComplexity, 44_100, 128_000, Transport::Adts),
        DecTransport::Adts,
        3.0,
    );
    let (_, he) = round_trip(
        params(AudioObjectType::Mpeg4HeAacV2, 44_100, 32_000, Transport::Adts),
        DecTransport::Adts,
        3.0,
    );
    println!("AAC-LC 128k: {} bytes, HE-AAC v2 32k: {} bytes", lc.bytes, he.bytes);
    assert!(he.bytes * 3 < lc.bytes, "HE-AAC v2 at 32 kbps should be far smaller than LC at 128");
}

/// `AudioObjectType::has_sbr` must agree with what the encoder does.
///
/// A convenience method that lies is worse than no method: a caller sizing buffers or
/// computing timestamps from it would be wrong in exactly the cases that are hardest to
/// notice.
#[test]
fn has_sbr_agrees_with_the_encoder() {
    for aot in [
        AudioObjectType::Mpeg4LowComplexity,
        AudioObjectType::Mpeg4HeAac,
        AudioObjectType::Mpeg4HeAacV2,
        AudioObjectType::Mpeg2Aac,
        AudioObjectType::Mpeg2HeAac,
    ] {
        let info =
            Encoder::new(params(aot, 32_000, 48_000, Transport::Adts)).unwrap().info().unwrap();
        // 2048 iff dual-rate SBR is in play.
        let encoder_says_sbr = info.frameLength == 2048;
        assert_eq!(
            aot.has_sbr(),
            encoder_says_sbr,
            "{aot:?}: has_sbr() says {} but the encoder's granule is {} samples",
            aot.has_sbr(),
            info.frameLength,
        );
    }
}

/// `Param::SignalingMode` applies on the transports that can express it, and is forced to
/// implicit on the ones that cannot.
///
/// Both halves are worth pinning. The first is the parameter working; the second is
/// fdk-aac's documented override (`aacenc_lib.cpp:421-426`), which is the reason
/// prebuilt.rs leaves `SignalingMode` out of its round-trip set — and which would otherwise
/// look like this crate failing to pass the value through.
#[test]
fn signaling_mode_applies_where_the_transport_allows_it() {
    let apply = |transport: Transport, requested: u32| -> u32 {
        let mut e =
            Encoder::new(params(AudioObjectType::Mpeg4HeAac, 32_000, 48_000, transport)).unwrap();
        e.set_param(Param::SignalingMode, requested).unwrap();
        // The encode applies the pending configuration; see `Encoder::get_param`.
        let info = e.info().unwrap();
        let pcm = vec![0i16; info.frameLength as usize * 2];
        let mut out = vec![0u8; info.maxOutBufBytes as usize];
        e.encode(&pcm, &mut out).unwrap();
        e.get_param(Param::SignalingMode)
    };

    // 2 = explicit hierarchical.
    assert_eq!(apply(Transport::Loas, 2), 2, "LOAS must honour explicit signalling");
    assert_eq!(apply(Transport::Raw, 1), 1, "raw must honour backward-compatible signalling");
    // ADTS overrides whatever it is given, because MPEG-2 transports have nowhere to put it.
    assert_eq!(
        apply(Transport::Adts, 2),
        0,
        "ADTS should force implicit signalling regardless of the request"
    );
}
