#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::result_large_err)]

use std::fs::File;
use std::io::{BufReader, Cursor};
use std::path::Path;

pub use akousma_core::meta::{Chapter, MetadataLog, MetadataRevision};
pub use akousma_core::{
    AudioBuf, AudioPlanes, ChannelMask, ChannelPosition, CodecId, CodecParams, CodecRegistry,
    DecodeOptions, Decoder, DemuxOptions, Demuxer, Error, ErrorKind, FormatLimits, Hint,
    IntegrityStatus, Packet, Probe, Probed, SeekMode, SeekTarget, SeekedTo, TimeBase, Track,
    TrackFlags,
};
pub use akousma_core::{audio, channel, codec, io, meta, pcm, probe, units};
/// Tag parsers (ID3v1, ID3v2, APE, Lyrics3 v2.00, RIFF `INFO`).
pub use akousma_meta as tags;
/// The WAVE demuxer and the ADPCM decoders (feature `wav`).
#[cfg(feature = "wav")]
pub use akousma_wav as wav;

use akousma_core::codec::CodecDescriptor;
use akousma_core::io::MediaSource;
use akousma_core::pcm::{PCM_CODECS, PcmDecoder};
use akousma_meta::anchors::TagScanner;

/// A [`Probe`] with every format enabled by the crate features and a scanner
/// for ID3v2, APE, ID3v1, `TAG+` and trailing Lyrics3 tags around the audio
/// data. It parses Lyrics3 v2.00 and locates Lyrics3 v1.00 blocks.
///
/// ```
/// let probe = akousma::default_probe();
/// # #[cfg(feature = "wav")]
/// assert!(probe.formats().iter().any(|format| format.name == "wav"));
/// ```
#[must_use]
pub fn default_probe() -> Probe {
    let mut probe = Probe::new();
    probe.set_anchor_scanner(Box::new(TagScanner));
    #[cfg(feature = "wav")]
    akousma_wav::register_demuxer(&mut probe);
    probe
}

/// A [`CodecRegistry`] with the PCM and G.711 decoders of `akousma-core` and
/// every decoder enabled by the crate features.
///
/// ```
/// use akousma::CodecId;
///
/// let codecs = akousma::default_codecs();
/// assert!(codecs.get(CodecId::PcmS16Le).is_some());
/// ```
#[must_use]
pub fn default_codecs() -> CodecRegistry {
    let mut registry = CodecRegistry::new();
    registry.register(CodecDescriptor {
        name: "pcm",
        codecs: PCM_CODECS,
        make: PcmDecoder::boxed,
    });
    #[cfg(feature = "wav")]
    akousma_wav::register_codecs(&mut registry);
    registry
}

/// A whole decoded track: interleaved samples and the metadata of the file.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DecodedAudio {
    /// Name of the container format, for example `"wav"`.
    pub format: &'static str,
    /// Parameters of the decoded track.
    pub params: CodecParams,
    /// Interleaved samples, `channels` per frame, in `-1.0..=1.0` for
    /// integer sources. Float sources may exceed that range.
    pub samples: Vec<f32>,
    /// Metadata revisions in source order: leading tags, container metadata,
    /// then trailing tags.
    pub metadata: Vec<MetadataRevision>,
    /// Chapters of the file.
    pub chapters: Vec<Chapter>,
}

impl DecodedAudio {
    /// Number of frames (samples per channel).
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.params.channels.max(1))
    }
}

/// Decodes the default track of the file at `path` with
/// [`default_probe`] and [`default_codecs`]. The file extension is used as a
/// hint.
///
/// The whole track is held in memory: use a [`Demuxer`] and a [`Decoder`]
/// to stream long files.
///
/// The samples are converted to `f32`: 32- and 64-bit integer sources and
/// 64-bit float sources can lose precision here. For integer PCM sources up to
/// 32 bits, use a [`Demuxer`] with an [`AudioBuf`] of integer planes to retain
/// the decoded precision. The 64-bit integer decoders keep only the most
/// significant 32 bits.
///
/// A container whose declared length runs past the end of the source is
/// truncated, not an error: the audio that is present decodes. Compare
/// [`DecodedAudio::frames`] with `params.total_frames` to notice.
///
/// # Errors
///
/// The errors of opening the file, of [`Probe::open`], of the decoder and
/// [`ErrorKind::NoAudioTrack`] if the file has no track.
///
/// Of an error of the file or of reading it, only its category is kept, as
/// an [`ErrorKind`]: [`Error`] has no room for the path or the text of the
/// operating system. For those, open the file yourself and pass it to
/// [`Probe::open`] of [`default_probe`]; [`MediaSource`] is implemented for
/// every `Read + Seek + Send` type.
pub fn decode_file(path: impl AsRef<Path>) -> Result<DecodedAudio, Error> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|err| Error::from(ErrorKind::from(err.kind())))?;
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint = hint.with_extension(extension);
    }
    decode_source(BufReader::new(file), &hint)
}

/// Decodes the default track of a file held in memory, like
/// [`decode_file`].
///
/// ```
/// # #[cfg(feature = "wav")] {
/// // 16-bit mono at 8 kHz with two frames.
/// let mut wav = b"RIFF\x28\x00\x00\x00WAVEfmt \x10\x00\x00\x00".to_vec();
/// wav.extend_from_slice(&[1, 0, 1, 0, 0x40, 0x1F, 0, 0, 0x80, 0x3E, 0, 0, 2, 0, 16, 0]);
/// wav.extend_from_slice(b"data\x04\x00\x00\x00\x00\x40\x00\xC0");
///
/// let decoded = akousma::decode_bytes(wav)?;
/// assert_eq!(decoded.format, "wav");
/// assert_eq!(decoded.params.sample_rate, 8_000);
/// assert_eq!(decoded.samples, [0.5, -0.5]);
/// # }
/// # Ok::<(), akousma::Error>(())
/// ```
///
/// # Errors
///
/// Same as [`decode_file`], without the file errors.
pub fn decode_bytes(bytes: impl Into<Vec<u8>>) -> Result<DecodedAudio, Error> {
    decode_source(Cursor::new(bytes.into()), &Hint::new())
}

fn decode_source<S: MediaSource + 'static>(source: S, hint: &Hint) -> Result<DecodedAudio, Error> {
    let Probed {
        mut demuxer,
        format,
    } = default_probe().open(hint, source, &DemuxOptions::new())?;
    let track = demuxer
        .default_track()
        .ok_or(ErrorKind::NoAudioTrack)?
        .clone();
    let mut decoder = default_codecs().make(&track.params, &DecodeOptions::new())?;
    let params = &track.params;
    let mut buf = AudioBuf::f32(
        params.channels.max(1),
        params.sample_rate,
        params.channel_mask,
    )?;
    let mut packet = Packet::default();
    let mut samples = Vec::new();
    while demuxer.next_packet(&mut packet)? {
        if packet.track_id != track.id {
            continue;
        }
        decoder.decode(&packet, &mut buf)?;
        let start = samples.len();
        samples.resize(start + buf.samples(), 0.0);
        buf.copy_interleaved_f32(&mut samples[start..]);
    }
    decoder.finalize()?;
    Ok(DecodedAudio {
        format,
        params: track.params,
        samples,
        metadata: demuxer.metadata().iter().cloned().collect(),
        chapters: demuxer.chapters().to_vec(),
    })
}
