//! Core building blocks of the Akousma audio decoding library.
//!
//! This crate has no dependencies and contains no `unsafe` code. It provides:
//!
//! * [`Error`]: a `Copy` error type of at most 24 bytes with the byte offset
//!   where the problem was detected;
//! * [`FormatLimits`], [`DemuxOptions`], [`DecodeOptions`]: explicit limits
//!   and switches with documented defaults;
//! * [`io`]: [`ByteReader`](io::ByteReader) with bounded reads of untrusted
//!   lengths, [`ScopedReader`](io::ScopedReader), and the 64-bit bit readers
//!   [`BitReaderMsb`](io::BitReaderMsb) and [`BitReaderLsb`](io::BitReaderLsb);
//! * [`checksum`]: [`IntegrityStatus`], the outcome of a whole-stream
//!   integrity check (the CRC and MD5 implementations are internal);
//! * [`units`]: the rational [`TimeBase`] and the [`SeekIndex`](units::SeekIndex);
//! * [`channel`] and [`audio`]: speaker positions and the planar [`AudioBuf`]
//!   with an `O(1)` gapless trim window;
//! * [`codec`]: [`CodecId`], [`CodecParams`], [`Track`], [`Packet`], the
//!   [`Decoder`] trait and the [`CodecRegistry`];
//! * [`probe`]: the [`Demuxer`] trait and the two-tier format detection of
//!   [`Probe`], which hands each demuxer only the audio range of its source;
//! * [`meta`]: the metadata model (tags, pictures, chapters, revisions);
//! * [`pcm`]: [`PcmDecoder`](pcm::PcmDecoder) for integer, float and G.711
//!   samples.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::result_large_err)]

pub mod audio;
pub mod channel;
pub mod checksum;
pub mod codec;
pub mod error;
pub mod io;
#[allow(dead_code)]
pub(crate) mod math;
pub mod meta;
pub mod options;
pub mod pcm;
pub mod probe;
pub mod units;
#[allow(dead_code)]
pub(crate) mod vlc;

pub use audio::{AudioBuf, AudioPlanes};
pub use channel::{ChannelMask, ChannelPosition};
pub use checksum::IntegrityStatus;
pub use codec::{CodecId, CodecParams, CodecRegistry, Decoder, Packet, Track, TrackFlags};
pub use error::{Error, ErrorKind, IoErrorKind};
pub use options::{DecodeOptions, DemuxOptions, FormatLimits};
pub use probe::{Demuxer, Hint, Probe, Probed};
pub use units::{SeekMode, SeekTarget, SeekedTo, TimeBase};
