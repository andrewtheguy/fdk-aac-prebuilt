//! Raw FFI for the Fraunhofer FDK AAC codec, linked from a prebuilt static archive.
//!
//! This crate compiles no C++. Compare upstream `fdk-aac-sys`, which vendors the whole FDK
//! tree and builds ~170 `.cpp` files with the `cc` crate on every clean build; here
//! `build.rs` finds an archive that was already built — by `./build.sh` locally, or by this
//! repository's release pipeline — verifies it against its own MANIFEST, and emits the link
//! flags. A consumer needs no C++ toolchain, no cmake and no autotools.
//!
//! Everything public comes from [`bindings`], which is bindgen's output over the six
//! headers committed in `include/fdk-aac/` and re-exported at the crate root so that
//! `fdk_aac_prebuilt_sys::aacEncOpen` reads the way `fdk_aac_sys::aacEncOpen` does.
//!
//! # Which library is linked
//!
//! [`version`] carries the encoder and decoder module versions lifted from the pinned
//! sources. fdk-aac reports no package version at runtime, so requiring
//! `aacEncGetLibInfo` to agree with these constants is how the crates above check they are
//! talking to the fdk-aac they were generated against rather than to a system library that
//! won the link. `crates/fdk-aac-prebuilt/tests/prebuilt.rs` does exactly that.
//!
//! # Safety
//!
//! Nothing here is safe. Handles are raw pointers with an open/close lifecycle, the buffer
//! descriptors passed to `aacEncEncode` hold pointers to pointers whose lifetimes the
//! compiler cannot see, and `aacDecoder_GetStreamInfo` returns a pointer into the decoder
//! that is invalidated by the next call. Use `fdk-aac-prebuilt` unless you have a reason
//! not to.

// bindgen's own header already carries the allow attributes these names need.
mod bindings;
pub mod version;

pub use bindings::*;
