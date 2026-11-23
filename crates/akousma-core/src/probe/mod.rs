//! Format detection: the [`Demuxer`] trait and the [`Probe`] registry.
//!
//! [`Probe::open`] turns an unknown byte source into a [`Demuxer`]:
//!
//! 1. An optional [`AnchorScanner`] reads the tags in front of the audio data
//!    (for example ID3v2 and APEv2) and returns where the audio data starts.
//! 2. A window of up to [`PROBE_WINDOW`] bytes from there, and never more
//!    than [`FormatLimits::max_probe_bytes`](crate::FormatLimits), is given
//!    to every registered [`FormatDescriptor`]. Formats of the
//!    [`Tier::Standard`] tier have an unambiguous container signature and
//!    are tried first. Only if none of them matches, formats of the
//!    [`Tier::Fallback`] tier (raw streams with a short sync word) are
//!    scored, and the best one wins if its score reaches
//!    [`MIN_FALLBACK_SCORE`]. If nothing matches in a window that the probe
//!    limit made shorter, the error is [`ErrorKind::LimitExceeded`], not
//!    [`ErrorKind::UnsupportedFormat`].
//! 3. If there is an anchor scanner and the chosen format can read the
//!    length of its container ([`FormatDescriptor::extent`]), that length is
//!    read through a [`ProbeReader`], within what is left of the probe
//!    limit. The scanner then looks for tags after the audio data (for
//!    example APE, ID3v1 and Lyrics3) only past the container, so that bytes
//!    inside it that happen to look like a tag are not taken for one. If
//!    the header declares no length ([`Extent::Unbounded`]), it searches the
//!    whole rest of the source. An error while reading the length (the limit
//!    is too small, the header is broken or cut, the source fails) is the
//!    error of [`Probe::open`]: it is never taken for a missing length.
//! 4. The chosen demuxer reads the source through a [`ScopedReader`]
//!    restricted to the audio range, so it never sees the wrapping tags as
//!    audio; the byte offsets of its errors are relative to the start of
//!    that range. In [`Demuxer::metadata`], the revisions of the tags in front of
//!    the audio data come first, then the container's own, then those of the
//!    tags after the audio data: the order of the source. Chapters of the
//!    leading tags are placed in front of the container's own through
//!    [`Demuxer::chapters_mut`].
//!
//! All the metadata of the source shares one [`MetadataBudget`], created
//! from [`DemuxOptions::limits`](crate::DemuxOptions::limits): the scanner
//! spends on the tags around the container, the demuxer gets the rest.

use std::io::{Seek, SeekFrom};

use crate::codec::{Packet, Track};
use crate::error::{Error, ErrorKind};
use crate::io::{MediaSource, ScopedReader};
use crate::meta::{Chapter, MetadataBudget, MetadataLog, MetadataRevision};
use crate::options::DemuxOptions;
use crate::units::{SeekMode, SeekTarget, SeekedTo};

mod reader;

pub use reader::{Extent, ProbeReader};

/// Number of bytes at the start of the audio range given to
/// [`FormatDescriptor::score`].
pub const PROBE_WINDOW: usize = 16 * 1024;

/// Smallest score with which a [`Tier::Fallback`] format is accepted.
///
/// Fallback formats should reach it only after several consecutive valid
/// frames, so that a stray sync word in other data is not taken for audio.
pub const MIN_FALLBACK_SCORE: Score = 100;

/// Confidence of a format in a byte window, `0` for no match.
pub type Score = u8;

/// A demultiplexer: reads the packets of the tracks of one container.
pub trait Demuxer: Send {
    /// Tracks of the container.
    fn tracks(&self) -> &[Track];

    /// The track to play by default: the first one unless the container says
    /// otherwise.
    fn default_track(&self) -> Option<&Track> {
        self.tracks().first()
    }

    /// Chapters of the container. When the demuxer was opened by
    /// [`Probe::open`], the chapters of the tags in front of the container
    /// come first.
    fn chapters(&self) -> &[Chapter];

    /// The list returned by [`Demuxer::chapters`], mutable.
    ///
    /// [`Probe::open`] puts the chapters of the tags in front of the
    /// container at its start, so a demuxer must keep a list even if its
    /// container has no chapters of its own, and must return the same list
    /// from both methods. The method has no default, so that no demuxer
    /// loses these chapters by omission.
    fn chapters_mut(&mut self) -> &mut Vec<Chapter>;

    /// Metadata read so far, in the order of the source. When the demuxer
    /// was opened by [`Probe::open`], the revisions of the tags in front of
    /// the container come first and those of the tags after it last.
    fn metadata(&mut self) -> &mut MetadataLog;

    /// Reads the next packet into `packet`, reusing its buffer.
    ///
    /// Returns `Ok(false)` at the end of the stream.
    ///
    /// # Errors
    ///
    /// Any error of the container or the source.
    fn next_packet(&mut self, packet: &mut Packet) -> Result<bool, Error>;

    /// Moves to `target`. Packets queued before the seek are dropped.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::TrackNotFound`], or any error of the container or the
    /// source.
    fn seek(&mut self, mode: SeekMode, target: SeekTarget) -> Result<SeekedTo, Error>;
}

/// A demuxer chosen by [`Probe::open`].
pub struct Probed {
    /// The demuxer, reading the audio range of the source.
    pub demuxer: Box<dyn Demuxer>,
    /// [`FormatDescriptor::name`] of the chosen format.
    pub format: &'static str,
}

impl core::fmt::Debug for Probed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Probed")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// What the caller knows about a source: file extension and MIME type.
///
/// Hints only break ties between formats that match equally well.
///
/// ```
/// use akousma_core::probe::Hint;
///
/// let hint = Hint::new().with_extension("FLAC").with_mime_type("audio/flac");
/// assert_eq!(hint.extension(), Some("FLAC"));
/// assert_eq!(hint.mime_type(), Some("audio/flac"));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hint {
    extension: Option<String>,
    mime_type: Option<String>,
}

impl Hint {
    /// A hint without information.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            extension: None,
            mime_type: None,
        }
    }

    /// Sets the file extension, without the dot.
    #[must_use]
    pub fn with_extension(mut self, extension: &str) -> Self {
        self.extension = Some(extension.to_owned());
        self
    }

    /// Sets the MIME type.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: &str) -> Self {
        self.mime_type = Some(mime_type.to_owned());
        self
    }

    /// The file extension, if set.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        self.extension.as_deref()
    }

    /// The MIME type, if set.
    #[must_use]
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// Returns `true` if the extension or the MIME type names `format`,
    /// ignoring ASCII case.
    fn names(&self, format: &FormatDescriptor) -> bool {
        let matches = |given: Option<&str>, known: &[&str]| {
            given.is_some_and(|given| known.iter().any(|k| k.eq_ignore_ascii_case(given)))
        };
        matches(self.extension(), format.extensions) || matches(self.mime_type(), format.mime_types)
    }
}

/// Detection tier of a format.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// A container with an unambiguous signature (for example `RIFF` and
    /// `WAVE`, `fLaC`, `OggS`). Any non-zero score is a match.
    Standard,
    /// A raw stream recognized by a short sync word (for example MP3 or ADTS
    /// AAC). Tried only if no standard format matches, and accepted from
    /// [`MIN_FALLBACK_SCORE`].
    Fallback,
}

/// The source a demuxer opened by [`Probe::open`] reads: the audio range of
/// the caller's source.
pub type ScopedSource = ScopedReader<Box<dyn MediaSource>>;

/// Opens a demuxer over the audio range of a source.
///
/// The [`MetadataBudget`] is what the tags around the container left of the
/// metadata limits of the source. The demuxer takes from it the stored size
/// of every metadata block it reads, and one entry per tag, picture and
/// chapter it keeps; see [`MetadataBudget`].
pub type DemuxerFactory =
    fn(ScopedSource, &DemuxOptions, &mut MetadataBudget) -> Result<Box<dyn Demuxer>, Error>;

/// Reads the length of a container whose header starts the audio range; see
/// [`FormatDescriptor::extent`].
pub type ExtentFn = fn(&mut ProbeReader<'_>) -> Result<Extent, Error>;

/// A container format that can be registered in a [`Probe`].
#[derive(Copy, Clone, Debug)]
pub struct FormatDescriptor {
    /// Short name, for example `"wav"`.
    pub name: &'static str,
    /// File extensions, without the dot.
    pub extensions: &'static [&'static str],
    /// MIME types.
    pub mime_types: &'static [&'static str],
    /// Detection tier.
    pub tier: Tier,
    /// Confidence that the window, which starts at the first byte of the
    /// audio range, holds this format; `0` for no match.
    pub score: fn(&[u8]) -> Score,
    /// Constructor.
    pub open: DemuxerFactory,
    /// Length in bytes of the container whose header starts the audio
    /// range, as the header declares it. [`Probe::open`] looks for tags
    /// after the audio data only past this length.
    ///
    /// The function reads what it needs through the [`ProbeReader`], within
    /// the probe limit, and returns [`Extent::Unbounded`] only if the header
    /// declares no length. An error (a limit too small for the bytes it
    /// needs, a broken or truncated header, an error of the source) makes
    /// [`Probe::open`] fail with it. `None` for formats without a length in
    /// their header.
    pub extent: Option<ExtentFn>,
}

/// Tags around the audio data and the byte range of the audio data, as
/// found by an [`AnchorScanner`].
///
/// Revisions are kept in the order of the source. That order says where a
/// tag block is, not when it was written: the library does not rank
/// conflicting values of different blocks, the caller chooses which block
/// takes precedence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Anchors {
    /// First byte of the audio data.
    pub audio_start: u64,
    /// One past the last byte of the audio data.
    pub audio_end: u64,
    /// Revisions of the tags in front of the audio data, in file order.
    pub leading: Vec<MetadataRevision>,
    /// Revisions of the tags after the audio data, in file order.
    pub trailing: Vec<MetadataRevision>,
    /// Chapters of the tags in front of the audio data, in file order.
    pub chapters: Vec<Chapter>,
}

/// Finds the tags that wrap the audio data of a source.
///
/// Implemented outside this crate by the tag readers; [`Probe`] only calls
/// it, in two steps: the tags in front of the audio data first, then, once
/// the format and the length of its container are known, the tags after it.
///
/// Tags are parsed into revisions only if [`DemuxOptions::read_metadata`] is
/// set; their extent is always used for the audio range. A malformed tag
/// should be left out of the result rather than fail the scan, so that the
/// audio stays playable. An error of the source is not a malformed tag:
/// a source that fails, or that ends inside the length it reported, should
/// fail the scan.
///
/// Both steps share the [`MetadataBudget`] of the source with the demuxer.
/// With [`DemuxOptions::read_metadata`], the stored size of every tag that
/// is read and one entry per tag, picture and chapter kept are taken from
/// it, and a tag that does not fit is [`ErrorKind::LimitExceeded`]: a
/// resource limit, not a malformed tag, so it fails the scan. Without it, a
/// tag whose extent depends on its contents is checked without being kept,
/// and the bytes read to check it are taken from the scanned bytes of the
/// budget ([`MetadataBudget::take_scan_bytes`]), so that the audio range is
/// the same with either value.
pub trait AnchorScanner: Send + Sync {
    /// Scans the start of `source`, whose length is `len`, and returns
    /// anchors with [`Anchors::audio_start`], [`Anchors::leading`] and
    /// [`Anchors::chapters`] set, [`Anchors::audio_end`] equal to `len` and
    /// no trailing revisions.
    ///
    /// # Errors
    ///
    /// Errors of the source, and [`ErrorKind::LimitExceeded`] if `budget`
    /// runs out.
    fn scan_leading(
        &self,
        source: &mut dyn MediaSource,
        len: u64,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<Anchors, Error>;

    /// Scans backwards from [`Anchors::audio_end`] for tags that start at
    /// or after `floor`, which is not below [`Anchors::audio_start`], moving
    /// `audio_end` to the start of the first one and setting
    /// [`Anchors::trailing`].
    ///
    /// # Errors
    ///
    /// Errors of the source, and [`ErrorKind::LimitExceeded`] if `budget`
    /// runs out.
    fn scan_trailing(
        &self,
        source: &mut dyn MediaSource,
        anchors: &mut Anchors,
        floor: u64,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<(), Error>;
}

/// Registry of container formats and the entry point of format detection.
///
/// ```
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::probe::{FormatDescriptor, Hint, Probe, ScopedSource, Tier, Demuxer};
/// use akousma_core::{DemuxOptions, Error, ErrorKind};
///
/// fn riff(window: &[u8]) -> u8 {
///     if window.starts_with(b"RIFF") { 255 } else { 0 }
/// }
/// fn unavailable(
///     _: ScopedSource,
///     _: &DemuxOptions,
///     _: &mut MetadataBudget,
/// ) -> Result<Box<dyn Demuxer>, Error> {
///     Err(ErrorKind::UnsupportedFeature.into())
/// }
///
/// let mut probe = Probe::new();
/// probe.register(FormatDescriptor {
///     name: "wav",
///     extensions: &["wav"],
///     mime_types: &["audio/wav"],
///     tier: Tier::Standard,
///     score: riff,
///     open: unavailable,
///     extent: None,
/// });
/// assert_eq!(probe.detect(&Hint::new(), b"RIFF\0\0\0\0WAVE").map(|f| f.name), Some("wav"));
/// assert!(probe.detect(&Hint::new(), b"fLaC").is_none());
/// ```
#[derive(Default)]
pub struct Probe {
    formats: Vec<FormatDescriptor>,
    scanner: Option<Box<dyn AnchorScanner>>,
}

impl core::fmt::Debug for Probe {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Probe")
            .field("formats", &self.formats)
            .field("scanner", &self.scanner.is_some())
            .finish()
    }
}

impl Probe {
    /// Creates a registry without formats and without an anchor scanner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            formats: Vec::new(),
            scanner: None,
        }
    }

    /// Adds a format. On equal score and equal hint match, the format
    /// registered first wins.
    pub fn register(&mut self, format: FormatDescriptor) {
        self.formats.push(format);
    }

    /// Sets the scanner of the tags around the audio data. Without one, the
    /// whole source is the audio range.
    pub fn set_anchor_scanner(&mut self, scanner: Box<dyn AnchorScanner>) {
        self.scanner = Some(scanner);
    }

    /// Registered formats, in registration order.
    #[must_use]
    pub fn formats(&self) -> &[FormatDescriptor] {
        &self.formats
    }

    /// Chooses the format of `window`, the first bytes of the audio range.
    ///
    /// Standard formats with a non-zero score are preferred; otherwise
    /// fallback formats with a score of at least [`MIN_FALLBACK_SCORE`]
    /// compete. Within a tier the highest score wins, then a format named by
    /// `hint`, then the earliest registered.
    #[must_use]
    pub fn detect(&self, hint: &Hint, window: &[u8]) -> Option<&FormatDescriptor> {
        let best = |tier: Tier, threshold: Score| {
            let mut best: Option<(&FormatDescriptor, (Score, bool))> = None;
            for format in self.formats.iter().filter(|f| f.tier == tier) {
                let score = (format.score)(window);
                if score == 0 || score < threshold {
                    continue;
                }
                let key = (score, hint.names(format));
                if best.is_none_or(|(_, best_key)| key > best_key) {
                    best = Some((format, key));
                }
            }
            best.map(|(format, _)| format)
        };
        best(Tier::Standard, 1).or_else(|| best(Tier::Fallback, MIN_FALLBACK_SCORE))
    }

    /// Detects the format of `source` and opens a demuxer for it.
    ///
    /// The source must be seekable. To decode a pipe or socket, first buffer
    /// its contents into a seekable reader such as a `Cursor`.
    ///
    /// The steps are described in the [module documentation](self).
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::UnsupportedFormat`] if no registered format matches a
    ///   whole window.
    /// - [`ErrorKind::LimitExceeded`] if no format matches a window that
    ///   [`FormatLimits::max_probe_bytes`](crate::FormatLimits) made shorter,
    ///   if the length of the container needs more bytes than that limit
    ///   leaves, or if the metadata does not fit in its [`MetadataBudget`].
    /// - The errors of [`FormatDescriptor::extent`], of the anchor scanner,
    ///   of the source and of the demuxer constructor.
    pub fn open<S: MediaSource + 'static>(
        &self,
        hint: &Hint,
        source: S,
        options: &DemuxOptions,
    ) -> Result<Probed, Error> {
        let mut source: Box<dyn MediaSource> = Box::new(source);
        let io_error = |err: std::io::Error| Error::from(ErrorKind::from(err.kind()));
        let len = source.seek(SeekFrom::End(0)).map_err(io_error)?;
        let mut budget = MetadataBudget::new(&options.limits);
        let mut anchors = match &self.scanner {
            Some(scanner) => scanner.scan_leading(&mut *source, len, options, &mut budget)?,
            None => Anchors {
                audio_start: 0,
                audio_end: len,
                ..Anchors::default()
            },
        };
        if anchors.audio_start > anchors.audio_end || anchors.audio_end > len {
            return Err(ErrorKind::InvalidData.into());
        }

        let available = anchors.audio_end - anchors.audio_start;
        let (format, extent) = {
            let mut reader = ProbeReader::new(
                &mut *source,
                anchors.audio_start,
                available,
                options.limits.max_probe_bytes,
            )?;
            let Some(format) = self.detect(hint, reader.window()) else {
                // The window is shorter than both the range and the
                // standard window only when the probe limit cut it.
                let whole = available.min(PROBE_WINDOW as u64);
                let cut = (reader.window().len() as u64) < whole;
                return Err(if cut {
                    ErrorKind::LimitExceeded
                } else {
                    ErrorKind::UnsupportedFormat
                }
                .into());
            };
            let extent = match (&self.scanner, format.extent) {
                (Some(_), Some(extent)) => extent(&mut reader)?,
                _ => Extent::Unbounded,
            };
            (format, extent)
        };
        if let Some(scanner) = &self.scanner {
            // Trailing tags start after the declared container, which may
            // reach past the end of a truncated source.
            let floor = match extent {
                Extent::Bounded(extent) => anchors.audio_start.saturating_add(extent).min(len),
                Extent::Unbounded => anchors.audio_start,
            };
            scanner.scan_trailing(&mut *source, &mut anchors, floor, options, &mut budget)?;
            if anchors.audio_end < anchors.audio_start || anchors.audio_end > len {
                return Err(ErrorKind::InvalidData.into());
            }
        }
        let scoped =
            ScopedReader::new(source, anchors.audio_start, anchors.audio_end).map_err(io_error)?;
        let mut demuxer = (format.open)(scoped, options, &mut budget)?;
        let log = demuxer.metadata();
        for revision in anchors.leading.into_iter().rev() {
            log.push_front(revision);
        }
        for revision in anchors.trailing {
            log.push(revision);
        }
        if !anchors.chapters.is_empty() {
            demuxer.chapters_mut().splice(0..0, anchors.chapters);
        }
        Ok(Probed {
            demuxer,
            format: format.name,
        })
    }
}

#[cfg(test)]
mod tests;
