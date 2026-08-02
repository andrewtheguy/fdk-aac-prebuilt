//! The shape of the API, asserted so it cannot be quietly changed back.
//!
//! These are compile-time checks with almost no runtime body. They exist because the fork's
//! headline fix — removing `unsafe impl Sync for EncoderHandle` — is one line, in a file
//! that will be re-synced against upstream the next time `fdk-aac` releases, and re-adding
//! it would restore a data race reachable from safe code while breaking nothing that any
//! test exercises. A trait bound is the only thing that notices.

use fdk_aac::dec::Decoder;
use fdk_aac::enc::{AudioObjectType, BitRate, ChannelMode, Encoder, EncoderParams, Transport};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

/// An encoder may move between threads. Encoding is a long, CPU-bound job and handing one to
/// a worker pool is the obvious thing to want.
#[test]
fn encoder_is_send() {
    assert_send::<Encoder>();
}

// An encoder may **not** be shared between threads, and that assertion lives where the
// compiler will actually run it: the `compile_fail` doc test on `Encoder` in src/enc.rs.
//
// It cannot live here. There is no negative trait bound to write — `assert_not_sync::<T>()`
// is not expressible — and `compile_fail` blocks are only collected by rustdoc from the
// *library* crate, never from an integration test file. A doc comment saying "this must not
// compile" in this file would be a comment, not a test, and would pass forever after
// somebody re-added `unsafe impl Sync`.

/// A decoder may be shared, and that is sound rather than an oversight: every method that
/// touches decoder state takes `&mut self`, and the two that take `&self` only read.
#[test]
fn decoder_is_send_and_sync() {
    assert_send::<Decoder>();
    assert_sync::<Decoder>();
}

/// `EncoderParams` is a plain value: copyable, comparable, and defaultable, so a caller can
/// build one configuration from another without repeating every field.
#[test]
fn encoder_params_is_a_value() {
    let base = EncoderParams::default();
    let mono = EncoderParams { channels: ChannelMode::Mono, ..base };
    assert_eq!(base.channels, ChannelMode::Stereo);
    assert_eq!(mono.channels, ChannelMode::Mono);
    // Copy, not move: `base` is still usable.
    assert_eq!(base.sample_rate, mono.sample_rate);
    assert_ne!(base, mono);
}

/// The errors are real `std::error::Error`s, so `?` works into a boxed error and anyhow and
/// thiserror can consume them.
///
/// FORK: upstream implemented neither, which meant every consumer wrote a wrapper type.
#[test]
fn errors_are_std_errors() {
    fn fallible() -> Result<(), Box<dyn std::error::Error>> {
        // A configuration fdk-aac rejects, so this really does take the error path.
        Encoder::new(EncoderParams { sample_rate: 96_001, ..Default::default() })?;
        Ok(())
    }
    let err = fallible().expect_err("96001 Hz should not be encodable");
    println!("boxed: {err}");
    assert!(!err.to_string().is_empty());
}

/// A move to another thread must work in practice, not just satisfy a bound.
#[test]
fn an_encoder_really_moves_between_threads() {
    let mut encoder = Encoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(64_000),
        sample_rate: 48_000,
        transport: Transport::Adts,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .unwrap();

    let handle = std::thread::spawn(move || {
        let info = encoder.info().unwrap();
        let pcm = vec![0i16; info.frameLength as usize];
        let mut out = vec![0u8; info.maxOutBufBytes as usize];
        for _ in 0..8 {
            encoder.encode(&pcm, &mut out).unwrap();
        }
        // Returned so the encoder is dropped on this thread, which is where `aacEncClose`
        // then runs — the case a `Send` that was wrong about thread affinity would break.
        info.frameLength
    });
    assert_eq!(handle.join().unwrap(), 1024);
}
