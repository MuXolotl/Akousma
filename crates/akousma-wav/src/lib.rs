//! WAVE support for the Akousma audio decoding library: a demuxer for the
//! five members of the WAVE family and decoders for the codecs they carry.
//!
//! | Container | Byte order | Sizes |
//! | :--- | :--- | :--- |
//! | RIFF | little-endian | 32-bit |
//! | RIFX | big-endian | 32-bit |
//! | RF64 (EBU Tech 3306) | little-endian | 64-bit through `ds64` |
//! | BW64 (ITU-R BS.2088) | little-endian | 64-bit through `ds64` |
//! | Sony Wave64 | little-endian | 64-bit |
//!
//! | Format tag | Codec | Decoder |
//! | :--- | :--- | :--- |
//! | `0x0001` PCM, 1–32-bit samples or samples in 64-bit containers | `PcmU8`, `PcmS16*`, `PcmS24*`, `PcmS32*`, `PcmS64*` | [`PcmDecoder`] |
//! | `0x0003` IEEE float, 32 or 64 bits | `PcmF32*`, `PcmF64*` | [`PcmDecoder`] |
//! | `0x0006` A-law, `0x0007` μ-law | `G711Alaw`, `G711Mulaw` | [`PcmDecoder`] |
//! | `0x0011` IMA ADPCM | `AdpcmImaWav` | [`AdpcmImaDecoder`] |
//! | `0x0002` Microsoft ADPCM | `AdpcmMs` | [`AdpcmMsDecoder`] |
//! | `0x0050` MPEG, `0x0055` MP3, `0x00FF` AAC, `0x704F` Opus, `0xF1AC` FLAC | `Mp2`, `Mp3`, `Aac`, `Opus`, `Flac` | none in this crate |
//! | `0xFFFE` extensible | any of the above, by its sub-format GUID | |
//!
//! Channels of an extensible format follow its speaker mask; mono and
//! stereo files without one get the mono and stereo masks. Samples are
//! stored in the canonical channel order already.
//!
//! With [`DemuxOptions::read_metadata`](akousma_core::DemuxOptions), the
//! demuxer reads the metadata chunks before and after the audio data:
//! `LIST`/`INFO` fields, ID3v2 tags in `id3 ` chunks, broadcast extension
//! (`bext`) fields, sampler (`smpl`), loop (`acid`) and instrument (`inst`)
//! parameters, and `iXML`, `axml` and `cart` chunks as raw fields. Cue
//! points with their `adtl` labels become chapters; of several cue points
//! with one identifier only the first counts, and so does the first title,
//! note and region of each identifier. Only chunks inside the root chunk are
//! read: see [`WavDemuxer`] for the size rules of the root and `data`
//! chunks, the shared metadata budget, and what is not checked without
//! `read_metadata`. The revisions follow the order of the chunks.
//!
//! ```
//! use std::io::Cursor;
//!
//! use akousma_core::audio::AudioBuf;
//! use akousma_core::channel::ChannelMask;
//! use akousma_core::codec::{Decoder, Packet};
//! use akousma_core::probe::Demuxer;
//! use akousma_core::{DecodeOptions, DemuxOptions};
//! use akousma_wav::{PcmDecoder, WavDemuxer};
//!
//! // 16-bit stereo at 8 kHz with two frames.
//! let mut wav = b"RIFF\x2C\x00\x00\x00WAVEfmt \x10\x00\x00\x00".to_vec();
//! wav.extend_from_slice(&[1, 0, 2, 0, 0x40, 0x1F, 0, 0, 0, 0x7D, 0, 0, 4, 0, 16, 0]);
//! wav.extend_from_slice(b"data\x08\x00\x00\x00");
//! wav.extend_from_slice(&[0x00, 0x80, 0xFF, 0x7F, 0x01, 0x00, 0xFF, 0xFF]);
//!
//! let mut demuxer = WavDemuxer::open(Cursor::new(wav), &DemuxOptions::new())?;
//! let params = &demuxer.tracks()[0].params;
//! assert_eq!(params.total_frames, Some(2));
//! let mut decoder = PcmDecoder::new(params, &DecodeOptions::new())?;
//!
//! let mut packet = Packet::default();
//! let mut buf = AudioBuf::i16(2, 8_000, ChannelMask::STEREO)?;
//! while demuxer.next_packet(&mut packet)? {
//!     decoder.decode(&packet, &mut buf)?;
//! }
//! assert_eq!(buf.plane::<i16>(0), Some(&[i16::MIN, 1][..]));
//! assert_eq!(buf.plane::<i16>(1), Some(&[i16::MAX, -1][..]));
//! # Ok::<(), akousma_core::Error>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::result_large_err)]

mod adpcm;
mod adpcm_ms;
mod chunks;
mod container;
mod demuxer;
mod fmt;

use akousma_core::codec::{CodecDescriptor, CodecId, CodecRegistry};
use akousma_core::meta::MetadataBudget;
use akousma_core::pcm::PCM_CODECS;
use akousma_core::probe::{Demuxer, FormatDescriptor, Probe, ScopedSource, Score, Tier};
use akousma_core::{DemuxOptions, Error};

pub use adpcm::AdpcmImaDecoder;
pub use adpcm_ms::AdpcmMsDecoder;
pub use akousma_core::pcm::PcmDecoder;
pub use container::WavContainer;
pub use demuxer::WavDemuxer;

/// Length of the RIFF, RIFX, RF64 and BW64 stream header.
const RIFF_HEADER_LEN: usize = 12;

/// Probe entry of the WAVE family.
///
/// A window is matched by its stream header alone: `RIFF`, `RIFX`, `RF64`
/// or `BW64` followed by a size and `WAVE`, or the Wave64 `riff` and `wave`
/// GUIDs. Other RIFF forms (`AVI `, `WEBP`) do not match.
///
/// The extent is the root size. For RIFF and RIFX, root sizes `0` and
/// `0xFFFF_FFFF` declare no bound. RF64 and BW64 use the size in `ds64` when
/// the root size is `0xFFFF_FFFF`; a root or resolved size of `0` is
/// unbounded. Wave64 uses its 64-bit root size. Tags after the audio data are
/// searched for only past a bounded extent. The `ds64` chunk is found by
/// following chunk headers from the first one, reading 8 bytes per header
/// within [`FormatLimits::max_probe_bytes`](akousma_core::FormatLimits) and
/// seeking over the chunks between them. If `ds64` is missing, broken or cut,
/// [`Probe::open`] fails instead of treating the container as unbounded.
pub const FORMAT: FormatDescriptor = FormatDescriptor {
    name: "wav",
    extensions: &["wav", "wave", "rf64", "bw64", "w64", "amb"],
    mime_types: &["audio/wav", "audio/x-wav", "audio/vnd.wave", "audio/wave"],
    tier: Tier::Standard,
    score,
    open,
    extent: Some(container::extent),
};

/// Decoders of this crate: [`PcmDecoder`], [`AdpcmImaDecoder`] and
/// [`AdpcmMsDecoder`].
pub const CODECS: [CodecDescriptor; 3] = [
    CodecDescriptor {
        name: "pcm",
        codecs: PCM_CODECS,
        make: PcmDecoder::boxed,
    },
    CodecDescriptor {
        name: "adpcm_ima_wav",
        codecs: &[CodecId::AdpcmImaWav],
        make: AdpcmImaDecoder::boxed,
    },
    CodecDescriptor {
        name: "adpcm_ms",
        codecs: &[CodecId::AdpcmMs],
        make: AdpcmMsDecoder::boxed,
    },
];

/// Registers the WAVE demuxer ([`FORMAT`]) in `probe`.
///
/// ```
/// use akousma_core::Probe;
///
/// let mut probe = Probe::new();
/// akousma_wav::register_demuxer(&mut probe);
/// assert_eq!(probe.formats()[0].name, "wav");
/// ```
pub fn register_demuxer(probe: &mut Probe) {
    probe.register(FORMAT);
}

/// Registers the decoders of this crate ([`CODECS`]) in `registry`.
///
/// ```
/// use akousma_core::{CodecId, CodecRegistry};
///
/// let mut registry = CodecRegistry::new();
/// akousma_wav::register_codecs(&mut registry);
/// assert_eq!(registry.get(CodecId::AdpcmMs).map(|d| d.name), Some("adpcm_ms"));
/// ```
pub fn register_codecs(registry: &mut CodecRegistry) {
    for descriptor in CODECS {
        registry.register(descriptor);
    }
}

fn score(window: &[u8]) -> Score {
    let riff = window.len() >= RIFF_HEADER_LEN
        && matches!(&window[..4], b"RIFF" | b"RIFX" | b"RF64" | b"BW64")
        && &window[8..12] == b"WAVE";
    let w64 = window.len() >= container::W64_ROOT_LEN as usize
        && window[..16] == container::GUID_W64_RIFF
        && window[24..40] == container::GUID_W64_WAVE;
    if riff || w64 { Score::MAX } else { 0 }
}

fn open(
    source: ScopedSource,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<Box<dyn Demuxer>, Error> {
    Ok(Box::new(WavDemuxer::open_with_budget(
        source, options, budget,
    )?))
}
