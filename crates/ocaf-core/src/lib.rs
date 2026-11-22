//! # OCAF core
//!
//! OCAF (Onood's Compressed Audio File) is a lossless audio container and
//! predictive codec. This crate implements the binary format, the codec, WAV
//! import/export and integrity verification.
//!
//! ## Quick start
//!
//! ```no_run
//! use ocaf_core::{decode_to_wav, encode_wav, EncodeOptions};
//! use std::fs::File;
//!
//! # fn main() -> ocaf_core::Result<()> {
//! let input = File::open("music.wav")?;
//! let output = File::create("music.ocaf")?;
//! encode_wav(input, output, &EncodeOptions::default())?;
//!
//! let input = File::open("music.ocaf")?;
//! let output = File::create("music.decoded.wav")?;
//! decode_to_wav(input, output)?;
//! # Ok(())
//! # }
//! ```
//!
//! The public API is intentionally small; see [`encoder`], [`decoder`] and
//! [`wav`] for the building blocks.

pub mod bitio;
pub mod crc32;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod float;
pub mod format;
pub mod lpc;
pub mod metadata;
pub mod pcm;
pub mod rice;
pub mod subframe;
pub mod wav;

pub use decoder::{decode_to_wav, probe, verify, DecodeSummary, Decoder, ProbeInfo, VerifyReport};
pub use encoder::{encode_wav, EncodeOptions, EncodeSummary, StereoMode};
pub use error::{Error, Result};
pub use format::{ChannelAssignment, SampleFormat, StreamHeader, StreamInfo};
pub use metadata::{Metadata, MetadataEntry};

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
