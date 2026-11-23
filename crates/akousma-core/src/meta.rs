//! Metadata model shared by container demuxers and tag readers.
//!
//! Every tag keeps the key and value exactly as stored ([`RawTag`]) next to an
//! optional normalized interpretation ([`StandardTag`]), so fields without a
//! standard meaning are never lost. Tags and pictures of one tag block form a
//! [`MetadataRevision`]; revisions are queued in a [`MetadataLog`] in the
//! order of the source. Chapters ([`Chapter`]) describe the timeline of a
//! whole file and are kept apart from revisions.

use std::collections::VecDeque;
use std::time::Duration;

use crate::error::ErrorKind;
use crate::options::FormatLimits;

/// A tag value as stored in the file.
#[derive(Clone, Debug, PartialEq)]
pub enum RawValue {
    /// A single text value.
    Text(String),
    /// Several text values stored in one field (for example an ID3v2.4 frame
    /// with null-separated strings).
    MultiText(Vec<String>),
    /// Bytes that are not text.
    Binary(Vec<u8>),
    /// A floating-point number.
    Float(f64),
    /// A signed integer.
    Int(i64),
    /// A boolean.
    Flag(bool),
}

/// A tag key and value as stored in the file.
#[derive(Clone, Debug, PartialEq)]
pub struct RawTag {
    /// Key as stored (for example `"TITLE"`, `"TXXX:WORK"` or `"INAM"`).
    pub key: String,
    /// Value as stored.
    pub value: RawValue,
}

/// Normalized meaning of a tag.
///
/// Gain values are metadata only: decoders never apply them.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum StandardTag {
    /// Title of the track.
    TrackTitle(String),
    /// Subtitle or version of the track.
    TrackSubtitle(String),
    /// Title of the album.
    Album(String),
    /// Main artist of the album.
    AlbumArtist(String),
    /// Artist of the track.
    Artist(String),
    /// Composer.
    Composer(String),
    /// Conductor.
    Conductor(String),
    /// Performer.
    Performer(String),
    /// Author of the lyrics.
    Lyricist(String),
    /// Genre.
    Genre(String),
    /// Free-form comment.
    Comment(String),
    /// Unsynchronized lyrics.
    Lyrics(String),
    /// Description of the content.
    Description(String),
    /// Publisher or record label.
    Publisher(String),
    /// Copyright notice.
    Copyright(String),
    /// Person or software that encoded the file.
    EncodedBy(String),
    /// Encoder settings.
    EncoderSettings(String),
    /// International Standard Recording Code.
    Isrc(String),
    /// Product barcode (UPC or EAN).
    Barcode(String),
    /// Catalog number.
    CatalogNumber(String),
    /// Language of the content.
    Language(String),
    /// Mood.
    Mood(String),
    /// Title of the larger work.
    Work(String),
    /// Name of the movement.
    MovementName(String),
    /// Position of the track.
    TrackNumber(u32),
    /// Number of tracks.
    TrackTotal(u32),
    /// Position of the disc.
    DiscNumber(u32),
    /// Number of discs.
    DiscTotal(u32),
    /// Position of the movement.
    MovementNumber(u32),
    /// Number of movements.
    MovementTotal(u32),
    /// Tempo in beats per minute.
    Bpm(f32),
    /// The album is a compilation of several artists.
    Compilation(bool),
    /// Release-date text. Common forms are `YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
    /// and `YYYY-MM-DDTHH:MM:SS`; metadata parsers may preserve other text.
    ReleaseDate(String),
    /// Recording-date text. Common forms are `YYYY`, `YYYY-MM`,
    /// `YYYY-MM-DD`, and `YYYY-MM-DDTHH:MM:SS`; metadata parsers may preserve
    /// other text.
    RecordingDate(String),
    /// ReplayGain track gain in dB, relative to the ReplayGain reference
    /// loudness.
    ReplayGainTrackGainDb(f32),
    /// ReplayGain track peak, linear (`1.0` is full scale).
    ReplayGainTrackPeak(f32),
    /// ReplayGain album gain in dB.
    ReplayGainAlbumGainDb(f32),
    /// ReplayGain album peak, linear.
    ReplayGainAlbumPeak(f32),
    /// Opus `R128_TRACK_GAIN` (RFC 7845): Q7.8 dB, relative to -23 LUFS.
    R128TrackGainQ78(i16),
    /// Opus `R128_ALBUM_GAIN` (RFC 7845): Q7.8 dB, relative to -23 LUFS.
    R128AlbumGainQ78(i16),
    /// Encoder delay in frames, for gapless playback.
    EncoderDelay(u32),
    /// Encoder padding in frames, for gapless playback.
    EncoderPadding(u32),
}

/// A tag: the stored form and its normalized meaning, if it has one.
#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    /// Key and value as stored.
    pub raw: RawTag,
    /// Normalized meaning, or `None` for fields without a standard meaning.
    pub standard: Option<StandardTag>,
}

/// Picture type, shared by ID3v2 `APIC` and FLAC `PICTURE` (RFC 9639,
/// section 8.8).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VisualKind {
    /// 0: other.
    Other,
    /// 1: 32x32 pixel PNG file icon.
    FileIcon,
    /// 2: other file icon.
    OtherFileIcon,
    /// 3: front cover.
    FrontCover,
    /// 4: back cover.
    BackCover,
    /// 5: leaflet page.
    Leaflet,
    /// 6: media label side (for example the label of a CD).
    Media,
    /// 7: lead artist, lead performer or soloist.
    LeadArtist,
    /// 8: artist or performer.
    Artist,
    /// 9: conductor.
    Conductor,
    /// 10: band or orchestra.
    Band,
    /// 11: composer.
    Composer,
    /// 12: lyricist or text writer.
    Lyricist,
    /// 13: recording location.
    RecordingLocation,
    /// 14: during recording.
    DuringRecording,
    /// 15: during performance.
    DuringPerformance,
    /// 16: movie or video screen capture.
    ScreenCapture,
    /// 17: a bright colored fish.
    BrightColoredFish,
    /// 18: illustration.
    Illustration,
    /// 19: band or artist logotype.
    BandLogo,
    /// 20: publisher or studio logotype.
    PublisherLogo,
    /// A code above 20, reserved by the specifications. Codes `0..=20`
    /// always map to the named variants.
    Reserved(u32),
}

impl VisualKind {
    /// The kind with the stored `code`.
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            0 => Self::Other,
            1 => Self::FileIcon,
            2 => Self::OtherFileIcon,
            3 => Self::FrontCover,
            4 => Self::BackCover,
            5 => Self::Leaflet,
            6 => Self::Media,
            7 => Self::LeadArtist,
            8 => Self::Artist,
            9 => Self::Conductor,
            10 => Self::Band,
            11 => Self::Composer,
            12 => Self::Lyricist,
            13 => Self::RecordingLocation,
            14 => Self::DuringRecording,
            15 => Self::DuringPerformance,
            16 => Self::ScreenCapture,
            17 => Self::BrightColoredFish,
            18 => Self::Illustration,
            19 => Self::BandLogo,
            20 => Self::PublisherLogo,
            reserved => Self::Reserved(reserved),
        }
    }

    /// The stored code of this kind.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Other => 0,
            Self::FileIcon => 1,
            Self::OtherFileIcon => 2,
            Self::FrontCover => 3,
            Self::BackCover => 4,
            Self::Leaflet => 5,
            Self::Media => 6,
            Self::LeadArtist => 7,
            Self::Artist => 8,
            Self::Conductor => 9,
            Self::Band => 10,
            Self::Composer => 11,
            Self::Lyricist => 12,
            Self::RecordingLocation => 13,
            Self::DuringRecording => 14,
            Self::DuringPerformance => 15,
            Self::ScreenCapture => 16,
            Self::BrightColoredFish => 17,
            Self::Illustration => 18,
            Self::BandLogo => 19,
            Self::PublisherLogo => 20,
            Self::Reserved(code) => code,
        }
    }
}

/// An embedded picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visual {
    /// What the picture shows.
    pub kind: VisualKind,
    /// Lowercase MIME type (for example `"image/jpeg"`), or `"-->"` when
    /// `data` holds a URL of the picture instead of the picture.
    pub media_type: String,
    /// Description.
    pub description: String,
    /// Width in pixels, if stored.
    pub width: Option<u32>,
    /// Height in pixels, if stored.
    pub height: Option<u32>,
    /// Bits per pixel, if stored.
    pub color_depth: Option<u32>,
    /// Number of palette colors for indexed images, if stored.
    pub indexed_colors: Option<u32>,
    /// Picture bytes, or URL bytes when `media_type` is `"-->"`.
    pub data: Vec<u8>,
}

/// A chapter of a track.
#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    /// Identifier as stored.
    pub id: String,
    /// Start time.
    pub start: Duration,
    /// End time, if stored.
    pub end: Option<Duration>,
    /// Byte offset of the start, if stored.
    pub byte_offset_start: Option<u64>,
    /// Byte offset of the end, if stored.
    pub byte_offset_end: Option<u64>,
    /// Tags of the chapter (for example its title).
    pub tags: Vec<Tag>,
    /// Pictures of the chapter.
    pub visuals: Vec<Visual>,
}

/// Tags and pictures read from one tag block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetadataRevision {
    /// Track the revision applies to, or `None` for the whole container.
    pub track_id: Option<u32>,
    /// Tags in stored order.
    pub tags: Vec<Tag>,
    /// Pictures in stored order.
    pub visuals: Vec<Visual>,
}

/// Queue of metadata revisions in the order of the source.
///
/// For a file, that is the order in which the tag blocks are stored: tags
/// in front of the container, the container's own, then tags after it.
/// Revisions read later while streaming are appended. The position of a
/// block in a file does not tell when it was written, so the log does not
/// merge or rank revisions with conflicting values: the caller decides
/// which block takes precedence. [`MetadataLog::latest`] is the last
/// revision of the queue, not necessarily the most recently written.
///
/// ```
/// use akousma_core::meta::{MetadataLog, MetadataRevision};
///
/// let mut log = MetadataLog::new();
/// log.push(MetadataRevision { track_id: Some(1), ..MetadataRevision::default() });
/// log.push_front(MetadataRevision::default());
/// assert_eq!(log.latest().and_then(|r| r.track_id), Some(1));
/// assert_eq!(log.pop().map(|r| r.track_id), Some(None));
/// assert_eq!(log.len(), 1);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetadataLog {
    revisions: VecDeque<MetadataRevision>,
}

impl MetadataLog {
    /// Creates an empty log.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            revisions: VecDeque::new(),
        }
    }

    /// Appends a revision read after every queued one.
    pub fn push(&mut self, revision: MetadataRevision) {
        self.revisions.push_back(revision);
    }

    /// Inserts a revision before every queued one, for tag blocks that
    /// precede the container (for example a leading ID3v2 tag found while
    /// probing).
    pub fn push_front(&mut self, revision: MetadataRevision) {
        self.revisions.push_front(revision);
    }

    /// Removes and returns the first revision.
    pub fn pop(&mut self) -> Option<MetadataRevision> {
        self.revisions.pop_front()
    }

    /// The last revision of the queue: for a file, the block stored last.
    #[must_use]
    pub fn latest(&self) -> Option<&MetadataRevision> {
        self.revisions.back()
    }

    /// Revisions from first to last.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &MetadataRevision> {
        self.revisions.iter()
    }

    /// Number of queued revisions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.revisions.len()
    }

    /// Returns `true` if no revision is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.revisions.is_empty()
    }
}

/// What is left of the metadata limits of one source while its tag blocks
/// and metadata chunks are read.
///
/// One budget covers every block of a source: the tags in front of the
/// container, the metadata chunks inside it and the tags after it.
/// [`Probe::open`](crate::Probe::open) creates it from
/// [`DemuxOptions::limits`](crate::DemuxOptions::limits), lets the anchor
/// scanner spend on the tags around the container, then hands the rest to
/// the demuxer. It holds three separate limits:
///
/// - **Input bytes**, from [`FormatLimits::max_metadata_bytes`]: the stored
///   size of every block read, taken before the block is read into memory.
///   A block counts once, in full, whether it is a whole tag or the payload
///   of a metadata chunk.
/// - **Entries**, at most [`MetadataBudget::MAX_ENTRIES`]: the tags,
///   pictures and chapters kept, the tags and pictures inside chapters
///   included.
/// - **Scanned bytes**, from [`FormatLimits::max_tag_scan_bytes`]: the bytes
///   a tag scanner reads to check tags that it does not keep, without
///   [`DemuxOptions::read_metadata`](crate::DemuxOptions::read_metadata).
///   They are counted apart from the input bytes, which are then not used.
///
/// Neither of the first two is a byte-exact bound on the memory of the
/// result: decoded text can be larger than the stored bytes (up to about
/// three bytes per stored byte for UTF-16 text), and every entry has a fixed
/// size of its own. The memory of the result is bounded by these two limits
/// together.
///
/// Running out is [`ErrorKind::LimitExceeded`]; the budget is then left
/// unchanged.
///
/// ```
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::{ErrorKind, FormatLimits};
///
/// let mut budget = MetadataBudget::new(&FormatLimits::new().with_max_metadata_bytes(30));
/// budget.take_bytes(22)?;
/// assert_eq!(budget.remaining_bytes(), 8);
/// assert_eq!(budget.take_bytes(9), Err(ErrorKind::LimitExceeded));
/// budget.take_bytes(8)?;
/// assert_eq!(budget.remaining_bytes(), 0);
/// # Ok::<(), ErrorKind>(())
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MetadataBudget {
    bytes: usize,
    entries: usize,
    scan_bytes: usize,
}

impl MetadataBudget {
    /// Largest number of entries (tags, pictures and chapters, with those
    /// inside chapters) kept from the metadata of one source.
    pub const MAX_ENTRIES: usize = 65_536;

    /// A full budget: [`FormatLimits::max_metadata_bytes`] bytes,
    /// [`MetadataBudget::MAX_ENTRIES`] entries and
    /// [`FormatLimits::max_tag_scan_bytes`] scanned bytes.
    #[must_use]
    pub const fn new(limits: &FormatLimits) -> Self {
        Self {
            bytes: limits.max_metadata_bytes,
            entries: Self::MAX_ENTRIES,
            scan_bytes: limits.max_tag_scan_bytes,
        }
    }

    /// Input bytes left.
    #[must_use]
    pub const fn remaining_bytes(&self) -> usize {
        self.bytes
    }

    /// Entries left.
    #[must_use]
    pub const fn remaining_entries(&self) -> usize {
        self.entries
    }

    /// Scanned bytes left.
    #[must_use]
    pub const fn remaining_scan_bytes(&self) -> usize {
        self.scan_bytes
    }

    /// Takes `len` bytes that a tag scanner is about to read to check a tag
    /// it does not keep.
    ///
    /// ```
    /// use akousma_core::meta::MetadataBudget;
    /// use akousma_core::{ErrorKind, FormatLimits};
    ///
    /// let limits = FormatLimits::new().with_max_tag_scan_bytes(4096);
    /// let mut budget = MetadataBudget::new(&limits);
    /// budget.take_scan_bytes(4000)?;
    /// assert_eq!(budget.take_scan_bytes(97), Err(ErrorKind::LimitExceeded));
    /// assert_eq!(budget.remaining_scan_bytes(), 96);
    /// // The other limits are separate.
    /// assert_eq!(budget.remaining_bytes(), limits.max_metadata_bytes);
    /// # Ok::<(), ErrorKind>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] if fewer than `len` scanned bytes are
    /// left.
    pub fn take_scan_bytes(&mut self, len: u64) -> Result<(), ErrorKind> {
        let len = usize::try_from(len).map_err(|_| ErrorKind::LimitExceeded)?;
        self.scan_bytes = self
            .scan_bytes
            .checked_sub(len)
            .ok_or(ErrorKind::LimitExceeded)?;
        Ok(())
    }

    /// Takes the `len` stored bytes of a block.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] if fewer than `len` bytes are left.
    pub fn take_bytes(&mut self, len: u64) -> Result<(), ErrorKind> {
        let len = usize::try_from(len).map_err(|_| ErrorKind::LimitExceeded)?;
        self.bytes = self
            .bytes
            .checked_sub(len)
            .ok_or(ErrorKind::LimitExceeded)?;
        Ok(())
    }

    /// Takes `count` entries.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] if fewer than `count` entries are left.
    pub fn take_entries(&mut self, count: usize) -> Result<(), ErrorKind> {
        self.entries = self
            .entries
            .checked_sub(count)
            .ok_or(ErrorKind::LimitExceeded)?;
        Ok(())
    }

    /// Takes one entry per tag and picture of `revision`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] if they do not fit.
    pub fn take_revision(&mut self, revision: &MetadataRevision) -> Result<(), ErrorKind> {
        let count = revision.tags.len().saturating_add(revision.visuals.len());
        self.take_entries(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_budget_limits() {
        let limits = FormatLimits::new().with_max_metadata_bytes(10);
        let mut budget = MetadataBudget::new(&limits);
        assert_eq!(budget.remaining_entries(), MetadataBudget::MAX_ENTRIES);
        assert_eq!(budget.take_bytes(11), Err(ErrorKind::LimitExceeded));
        assert_eq!(budget.remaining_bytes(), 10);
        assert_eq!(budget.take_bytes(u64::MAX), Err(ErrorKind::LimitExceeded));
        budget.take_bytes(10).unwrap();
        assert_eq!(budget.take_bytes(1), Err(ErrorKind::LimitExceeded));
        budget.take_bytes(0).unwrap();

        let rev = revision(None, "a");
        budget
            .take_entries(MetadataBudget::MAX_ENTRIES - 1)
            .unwrap();
        budget.take_revision(&rev).unwrap();
        assert_eq!(budget.remaining_entries(), 0);
        assert_eq!(budget.take_revision(&rev), Err(ErrorKind::LimitExceeded));
        assert_eq!(budget.take_entries(1), Err(ErrorKind::LimitExceeded));
        budget.take_entries(0).unwrap();

        // Scanned bytes are a third, separate limit.
        let limits = FormatLimits::new()
            .with_max_metadata_bytes(0)
            .with_max_tag_scan_bytes(5);
        let mut budget = MetadataBudget::new(&limits);
        assert_eq!(budget.take_scan_bytes(6), Err(ErrorKind::LimitExceeded));
        assert_eq!(
            budget.take_scan_bytes(u64::MAX),
            Err(ErrorKind::LimitExceeded)
        );
        assert_eq!(budget.remaining_scan_bytes(), 5);
        budget.take_scan_bytes(5).unwrap();
        assert_eq!(budget.take_scan_bytes(1), Err(ErrorKind::LimitExceeded));
        assert_eq!(
            (budget.remaining_bytes(), budget.remaining_entries()),
            (0, MetadataBudget::MAX_ENTRIES)
        );
    }

    #[test]
    fn visual_kind_codes() {
        // Codes from the picture type table of RFC 9639, section 8.8.
        let table = [
            (0, VisualKind::Other),
            (1, VisualKind::FileIcon),
            (2, VisualKind::OtherFileIcon),
            (3, VisualKind::FrontCover),
            (4, VisualKind::BackCover),
            (5, VisualKind::Leaflet),
            (6, VisualKind::Media),
            (7, VisualKind::LeadArtist),
            (8, VisualKind::Artist),
            (9, VisualKind::Conductor),
            (10, VisualKind::Band),
            (11, VisualKind::Composer),
            (12, VisualKind::Lyricist),
            (13, VisualKind::RecordingLocation),
            (14, VisualKind::DuringRecording),
            (15, VisualKind::DuringPerformance),
            (16, VisualKind::ScreenCapture),
            (17, VisualKind::BrightColoredFish),
            (18, VisualKind::Illustration),
            (19, VisualKind::BandLogo),
            (20, VisualKind::PublisherLogo),
        ];
        for (code, kind) in table {
            assert_eq!(VisualKind::from_code(code), kind);
            assert_eq!(kind.code(), code);
        }
        for code in [21, 255, 256, u32::MAX] {
            assert_eq!(VisualKind::from_code(code), VisualKind::Reserved(code));
            assert_eq!(VisualKind::Reserved(code).code(), code);
        }
    }

    fn revision(track_id: Option<u32>, title: &str) -> MetadataRevision {
        MetadataRevision {
            track_id,
            tags: vec![Tag {
                raw: RawTag {
                    key: "TITLE".into(),
                    value: RawValue::Text(title.into()),
                },
                standard: Some(StandardTag::TrackTitle(title.into())),
            }],
            visuals: Vec::new(),
        }
    }

    #[test]
    fn metadata_log_order() {
        let mut log = MetadataLog::new();
        assert!(log.is_empty());
        assert_eq!(log.latest(), None);
        assert_eq!(log.pop(), None);

        log.push(revision(None, "container"));
        log.push(revision(Some(2), "chained"));
        log.push_front(revision(None, "leading tag"));
        assert_eq!(log.len(), 3);
        assert_eq!(log.latest(), Some(&revision(Some(2), "chained")));
        let ids: Vec<_> = log.iter().map(|r| r.track_id).collect();
        assert_eq!(ids, [None, None, Some(2)]);

        assert_eq!(log.pop(), Some(revision(None, "leading tag")));
        assert_eq!(log.pop(), Some(revision(None, "container")));
        assert_eq!(log.pop(), Some(revision(Some(2), "chained")));
        assert!(log.is_empty());
    }
}
