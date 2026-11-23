//! ID3v2.2, ID3v2.3 and ID3v2.4 tags.
//!
//! A tag is a 10-byte header, an optional extended header, a list of frames
//! and padding:
//!
//! | Bytes | Field |
//! | :--- | :--- |
//! | `0..3` | `"ID3"` |
//! | `3` | major version: `2`, `3` or `4` |
//! | `4` | revision, below `0xFF` |
//! | `5` | flags: `0x80` unsynchronisation, `0x40` extended header (ID3v2.2: compression), `0x10` footer (ID3v2.4 only) |
//! | `6..10` | size of everything after the header, without the footer, as a synchsafe integer (7 bits per byte) |
//!
//! [`parse`] reads a whole tag. It follows the rules of each version:
//!
//! - **Unsynchronisation** (`FF 00` stored for `FF`) is removed from the
//!   whole tag body in ID3v2.2 and ID3v2.3, and frame by frame in ID3v2.4,
//!   where the tag flag means that every frame is unsynchronised. It is
//!   never removed twice.
//! - The **extended header** takes `4 + size` bytes in ID3v2.3, where its
//!   size is a plain `u32`, and `size` bytes in ID3v2.4, where its size is a
//!   synchsafe integer that counts itself.
//! - **Frame headers** are 6 bytes in ID3v2.2 (3-character identifier, 24-bit
//!   size) and 10 bytes in ID3v2.3 and ID3v2.4 (identifier, size, two flag
//!   bytes). The size is a plain `u32` in ID3v2.3 and a synchsafe integer in
//!   ID3v2.4. Some writers stored plain sizes in ID3v2.4 tags: a size with a
//!   byte of `0x80` or more is read as plain, and a size whose synchsafe and
//!   plain readings differ is read as synchsafe if the synchsafe reading ends
//!   at the end of the frames, at padding or at a plausible frame header, as
//!   plain if only the plain reading does, and otherwise ends the frame list.
//! - Optional fields follow the frame header in the order of the version:
//!   decompressed size, encryption method and group in ID3v2.3; group,
//!   encryption method and data length in ID3v2.4. Compressed or encrypted
//!   frames are kept as [`RawValue::Binary`].
//! - Frames of size zero, frames with an invalid identifier and frames whose
//!   optional fields do not fit are skipped. A frame that does not fit in the
//!   tag ends the frame list; the frames before it are kept.
//! - Identifiers of ID3v2.2, also when an ID3v2.3 tag stores them followed by
//!   a `0x00` byte, are replaced by their ID3v2.4 equivalents (`TT2` becomes
//!   `TIT2`).
//!
//! The content of each frame is described in the table below. Every
//! frame becomes one or more [`Tag`]s whose raw key is the frame identifier,
//! followed by `:` and the description for `TXXX`, `WXXX`, `COMM`, `USLT` and
//! `RVA2` frames with a non-empty description. A frame whose content is
//! malformed is kept as [`RawValue::Binary`]. Duplicate frames are kept as
//! they are; they are never merged.
//!
//! | Frame | Raw value | Normalized |
//! | :--- | :--- | :--- |
//! | text `T***` | text, or a text list for several `0x00`-separated values | from the first value: `TIT2` [`TrackTitle`](StandardTag::TrackTitle), `TIT3` [`TrackSubtitle`](StandardTag::TrackSubtitle), `TALB` [`Album`](StandardTag::Album), `TPE1` [`Artist`](StandardTag::Artist), `TPE2` [`AlbumArtist`](StandardTag::AlbumArtist), `TPE3` [`Conductor`](StandardTag::Conductor), `TCOM` [`Composer`](StandardTag::Composer), `TEXT` [`Lyricist`](StandardTag::Lyricist), `TPUB` [`Publisher`](StandardTag::Publisher), `TCOP` [`Copyright`](StandardTag::Copyright), `TENC` [`EncodedBy`](StandardTag::EncodedBy), `TSSE` [`EncoderSettings`](StandardTag::EncoderSettings), `TSRC` [`Isrc`](StandardTag::Isrc), `TLAN` [`Language`](StandardTag::Language), `TMOO` [`Mood`](StandardTag::Mood), `MVNM` [`MovementName`](StandardTag::MovementName), `TRCK`, `TPOS`, `MVIN` (`n` or `n/total`) [`TrackNumber`](StandardTag::TrackNumber), [`DiscNumber`](StandardTag::DiscNumber), [`MovementNumber`](StandardTag::MovementNumber), `TBPM` [`Bpm`](StandardTag::Bpm) (a positive number such as `128` or `128.5`), `TCMP` [`Compilation`](StandardTag::Compilation) for `1` or `0`, `TYER` and `TDRC` [`RecordingDate`](StandardTag::RecordingDate) and `TDRL` [`ReleaseDate`](StandardTag::ReleaseDate) if the value matches a supported date form, `TCON` [`Genre`](StandardTag::Genre) with ID3v1 genre numbers (`"17"`, `"(17)"`) replaced by their names |
//! | `TXXX` | text, or a text list | by description, without regard to ASCII case: `REPLAYGAIN_TRACK_GAIN`, `REPLAYGAIN_TRACK_PEAK`, `REPLAYGAIN_ALBUM_GAIN`, `REPLAYGAIN_ALBUM_PEAK`, `ALBUMARTIST`, `WORK`, `BARCODE`, `CATALOGNUMBER`, and `iTunSMPB` (below) |
//! | `W***`, `WXXX` | URL text | none |
//! | `COMM` | text | [`Comment`](StandardTag::Comment) without description, `iTunSMPB` (below) |
//! | `USLT` | text | [`Lyrics`](StandardTag::Lyrics) |
//! | `RVA2` | binary adjustment data | [`ReplayGainTrackGainDb`](StandardTag::ReplayGainTrackGainDb), or [`ReplayGainAlbumGainDb`](StandardTag::ReplayGainAlbumGainDb) for the identification `album`, from the master volume channel: `i16 / 512` dB |
//! | `APIC`, `PIC` | a [`Visual`](akousma_core::meta::Visual) | |
//! | `CHAP`, `CTOC` | a [`Chapter`] per `CHAP`, in the order of the top-level ordered `CTOC` if present | |
//! | other | binary | none |
//!
//! An `iTunSMPB` value holds hexadecimal words; the second is the encoder
//! delay and the third the padding, in samples. It gives two tags with the
//! same raw key and value, normalized to
//! [`EncoderDelay`](StandardTag::EncoderDelay) and
//! [`EncoderPadding`](StandardTag::EncoderPadding).

use akousma_core::meta::{Chapter, MetadataBudget, MetadataRevision, RawValue};
use akousma_core::{Error, ErrorKind, FormatLimits};

use crate::new_tag;

mod frames;
mod sync;
mod text;

#[cfg(test)]
use sync::to_synchsafe;
use sync::{remove_unsync, synchsafe};

#[cfg(doc)]
use akousma_core::meta::{StandardTag, Tag};

/// First bytes of an ID3v2 tag.
pub const MAGIC: [u8; 3] = *b"ID3";

/// Length of the tag header, and of the ID3v2.4 footer.
pub const HEADER_LEN: usize = 10;

/// Header flag: the tag is unsynchronised.
const FLAG_UNSYNC: u8 = 0x80;

/// Header flag: an extended header follows (ID3v2.3 and ID3v2.4); the tag
/// is compressed (ID3v2.2).
const FLAG_EXTENDED: u8 = 0x40;

/// Header flag: a footer follows the tag (ID3v2.4 only).
const FLAG_FOOTER: u8 = 0x10;

/// Levels of frames: top-level frames, and the sub-frames of `CHAP` and
/// `CTOC` frames. Sub-frames are never searched for chapters.
const MAX_SUBFRAME_DEPTH: usize = 2;

/// The 10-byte header of an ID3v2 tag.
///
/// ```
/// use akousma_meta::id3v2::Id3v2Header;
///
/// let header = Id3v2Header::parse(b"ID3\x04\x00\x10\x00\x00\x02\x01")?;
/// assert_eq!((header.major, header.size), (4, 257));
/// assert_eq!(header.total_len(), 10 + 257 + 10); // with the footer
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Id3v2Header {
    /// Major version: 2, 3 or 4.
    pub major: u8,
    /// Revision.
    pub revision: u8,
    /// Flags.
    pub flags: u8,
    /// Size of the tag after the header, without the footer.
    pub size: u32,
}

impl Id3v2Header {
    /// Parses a tag header.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidHeader`] if `bytes` do not start with [`MAGIC`],
    /// if the major version is not 2, 3 or 4, if the revision is `0xFF`, or
    /// if a size byte is `0x80` or more.
    pub fn parse(bytes: &[u8; HEADER_LEN]) -> Result<Self, Error> {
        let [i, d, three, major, revision, flags, size @ ..] = *bytes;
        if [i, d, three] != MAGIC {
            return Err(Error::at(ErrorKind::InvalidHeader, 0));
        }
        if !(2..=4).contains(&major) || revision == 0xFF {
            return Err(Error::at(ErrorKind::InvalidHeader, 3));
        }
        let size = synchsafe(size).ok_or(Error::at(ErrorKind::InvalidHeader, 6))?;
        Ok(Self {
            major,
            revision,
            flags,
            size,
        })
    }

    /// Returns `true` if a 10-byte footer follows the tag. Only ID3v2.4 has
    /// a footer; the flag is ignored in earlier versions.
    #[must_use]
    pub const fn has_footer(&self) -> bool {
        self.major == 4 && self.flags & FLAG_FOOTER != 0
    }

    /// Number of bytes of the whole tag: header, body and footer (if any).
    #[must_use]
    pub const fn total_len(&self) -> u64 {
        let footer = if self.has_footer() { HEADER_LEN } else { 0 };
        (HEADER_LEN + footer) as u64 + self.size as u64
    }
}

/// Fields and chapters of an ID3v2 tag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Id3v2Tag {
    /// Fields and pictures.
    pub revision: MetadataRevision,
    /// Chapters from `CHAP` frames.
    pub chapters: Vec<Chapter>,
}

/// Parses an ID3v2 tag.
///
/// `tag` starts with the header and holds at least the [`Id3v2Header::size`]
/// bytes that follow it; bytes after them are ignored. An ID3v2.2 tag with
/// the compression flag is skipped: its result is empty.
///
/// ```
/// use akousma_core::meta::StandardTag;
/// use akousma_meta::id3v2;
///
/// let mut tag = b"ID3\x03\x00\x00\x00\x00\x00\x11".to_vec();
/// tag.extend_from_slice(b"TIT2\x00\x00\x00\x07\x00\x00\x00Title\x00");
/// let parsed = id3v2::parse(&tag)?;
/// assert_eq!(parsed.revision.tags[0].raw.key, "TIT2");
/// assert_eq!(parsed.revision.tags[0].standard, Some(StandardTag::TrackTitle("Title".into())));
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// - The errors of [`Id3v2Header::parse`].
/// - [`ErrorKind::UnexpectedEof`] if `tag` is shorter than the size in its
///   header.
/// - [`ErrorKind::InvalidData`] if the extended header does not fit in the
///   tag or, in ID3v2.4, if its size is not synchsafe or below 6.
/// - [`ErrorKind::LimitExceeded`] for more than
///   [`MetadataBudget::MAX_ENTRIES`] entries (fields, pictures and chapters
///   together, the fields and pictures inside chapters included), or a field
///   with more than 65 536 values.
pub fn parse(tag: &[u8]) -> Result<Id3v2Tag, Error> {
    parse_with_budget(tag, &mut MetadataBudget::new(&FormatLimits::new()))
}

/// Parses an ID3v2 tag like [`parse`], taking one entry of `budget` per
/// field, picture and chapter, the fields and pictures inside chapters
/// included.
///
/// Each entry is taken before it is stored, so several tags of one source
/// share the entries of its budget. The stored size of the tag is not taken:
/// the reader that loads `tag` into memory takes it first. On error the
/// budget is left unchanged.
///
/// ```
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::{ErrorKind, FormatLimits};
/// use akousma_meta::id3v2;
///
/// let mut tag = b"ID3\x03\x00\x00\x00\x00\x00\x22".to_vec();
/// for _ in 0..2 {
///     tag.extend_from_slice(b"TIT2\x00\x00\x00\x07\x00\x00\x00Title\x00");
/// }
/// let mut budget = MetadataBudget::new(&FormatLimits::new());
/// budget.take_entries(MetadataBudget::MAX_ENTRIES - 3)?;
/// assert_eq!(id3v2::parse_with_budget(&tag, &mut budget)?.revision.tags.len(), 2);
/// assert_eq!(budget.remaining_entries(), 1);
/// let err = id3v2::parse_with_budget(&tag, &mut budget).unwrap_err();
/// assert_eq!(err.kind, ErrorKind::LimitExceeded);
/// assert_eq!(budget.remaining_entries(), 1);
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// The errors of [`parse`], with [`ErrorKind::LimitExceeded`] once the
/// entries of `budget` run out.
pub fn parse_with_budget(tag: &[u8], budget: &mut MetadataBudget) -> Result<Id3v2Tag, Error> {
    let header_bytes = tag
        .first_chunk::<HEADER_LEN>()
        .ok_or(Error::at(ErrorKind::UnexpectedEof, 0))?;
    let header = Id3v2Header::parse(header_bytes)?;
    let body = usize::try_from(header.size)
        .ok()
        .and_then(|size| tag.get(HEADER_LEN..HEADER_LEN.checked_add(size)?))
        .ok_or(Error::at(ErrorKind::UnexpectedEof, HEADER_LEN as u64))?;
    if header.major == 2 && header.flags & FLAG_EXTENDED != 0 {
        return Ok(Id3v2Tag::default());
    }
    let tag_unsync = header.flags & FLAG_UNSYNC != 0;
    let mut body = body.to_vec();
    if tag_unsync && header.major <= 3 {
        remove_unsync(&mut body);
    }
    let frames_start = if header.major >= 3 && header.flags & FLAG_EXTENDED != 0 {
        extended_header_len(&body, header.major)
            .ok_or(Error::at(ErrorKind::InvalidData, HEADER_LEN as u64))?
    } else {
        0
    };
    let mut out = Frames::new(budget.remaining_entries());
    let format = Format {
        major: header.major,
        unsync_frames: tag_unsync && header.major == 4,
    };
    parse_frames(&body[frames_start..], format, 1, &mut out)
        .map_err(|kind| Error::at(kind, HEADER_LEN as u64))?;
    // Not above what was left, which `Frames::take_entry` never exceeds.
    budget.take_entries(budget.remaining_entries() - out.room)?;
    let mut chapters = out.chapters;
    if let Some(order) = out.order {
        chapters = frames::ordered(chapters, &order);
    }
    Ok(Id3v2Tag {
        revision: MetadataRevision {
            tags: out.revision.tags,
            visuals: out.revision.visuals,
            ..MetadataRevision::default()
        },
        chapters,
    })
}

/// Number of bytes taken by the extended header at the start of `body`, or
/// `None` if it does not fit or is malformed.
fn extended_header_len(body: &[u8], major: u8) -> Option<usize> {
    let size = *body.first_chunk::<4>()?;
    let len = if major == 3 {
        usize::try_from(u32::from_be_bytes(size))
            .ok()?
            .checked_add(4)?
    } else {
        let len = usize::try_from(synchsafe(size)?).ok()?;
        (len >= 6).then_some(len)?
    };
    (len <= body.len()).then_some(len)
}

/// How the frames of a tag are stored.
#[derive(Copy, Clone, Debug)]
struct Format {
    /// Major version of the tag.
    major: u8,
    /// ID3v2.4 tag flag: every frame is unsynchronised.
    unsync_frames: bool,
}

/// What the frames of a tag, or the sub-frames of a frame, contain.
struct Frames {
    /// Fields and pictures.
    revision: MetadataRevision,
    /// Chapters, in frame order.
    chapters: Vec<Chapter>,
    /// Element identifiers of the top-level ordered table of contents.
    order: Option<Vec<String>>,
    /// Entries (fields, pictures, chapters) that can still be stored, shared
    /// with the sub-frames of chapters.
    room: usize,
}

impl Frames {
    /// Empty contents with room for `room` entries.
    const fn new(room: usize) -> Self {
        Self {
            revision: MetadataRevision {
                track_id: None,
                tags: Vec::new(),
                visuals: Vec::new(),
            },
            chapters: Vec::new(),
            order: None,
            room,
        }
    }

    /// Takes room for one entry, before it is stored.
    fn take_entry(&mut self) -> Result<(), ErrorKind> {
        self.room = self.room.checked_sub(1).ok_or(ErrorKind::LimitExceeded)?;
        Ok(())
    }
}

/// A frame header.
struct FrameHeader {
    /// Identifier, mapped to ID3v2.4.
    id: [u8; 4],
    /// The identifier was stored with three characters.
    short_id: bool,
    /// Number of bytes after the header.
    size: usize,
    /// Format flags (second flag byte); `0` in ID3v2.2.
    format_flags: u8,
}

/// Parses the frames in `data` into `out`. `depth` is `1` for the frames of
/// the tag and grows by one for sub-frames.
fn parse_frames(
    data: &[u8],
    format: Format,
    depth: usize,
    out: &mut Frames,
) -> Result<(), ErrorKind> {
    let header_len = if format.major == 2 { 6 } else { 10 };
    let mut pos = 0;
    while let Some(raw) = data.get(pos..pos + header_len) {
        if raw[0] == 0 {
            break; // padding
        }
        let Some(header) = frame_header(data, pos, format.major) else {
            break;
        };
        let content_start = pos + header_len;
        let Some(content) = data.get(content_start..content_start + header.size) else {
            break;
        };
        pos = content_start + header.size;
        if header.size == 0 || !valid_id(&header.id) {
            continue;
        }
        let Some((payload, opaque)) = frame_payload(content, header.format_flags, format) else {
            continue;
        };
        let Ok(id) = core::str::from_utf8(&header.id) else {
            continue;
        };
        let id = id.trim_end_matches('\0');
        if opaque {
            out.take_entry()?;
            let value = RawValue::Binary(payload.into_owned());
            out.revision.tags.push(new_tag(id, value, None));
            continue;
        }
        let frame = frames::Frame {
            id,
            short_id: header.short_id,
            payload: &payload,
        };
        frames::add_frame(&frame, format, depth, out)?;
    }
    Ok(())
}

/// Reads the frame header at `pos`, or returns `None` to end the frame list.
fn frame_header(data: &[u8], pos: usize, major: u8) -> Option<FrameHeader> {
    if major == 2 {
        let [a, b, c, s0, s1, s2] = *data.get(pos..)?.first_chunk::<6>()?;
        let id = frames::upgrade_id([a, b, c]).unwrap_or([a, b, c, 0]);
        let size = usize::try_from(u32::from_be_bytes([0, s0, s1, s2])).ok()?;
        return Some(FrameHeader {
            id,
            short_id: true,
            size,
            format_flags: 0,
        });
    }
    let raw = data.get(pos..)?.first_chunk::<10>()?;
    let [a, b, c, d, s0, s1, s2, s3, _status, format_flags] = *raw;
    let mut id = [a, b, c, d];
    let mut short_id = false;
    if d == 0 && major == 3 && valid_id(&[a, b, c, b'0']) {
        id = frames::upgrade_id([a, b, c]).unwrap_or(id);
        short_id = true;
    }
    let size_bytes = [s0, s1, s2, s3];
    let size = if major == 3 {
        u32::from_be_bytes(size_bytes)
    } else {
        frame_size_v24(data, pos, size_bytes)?
    };
    Some(FrameHeader {
        id,
        short_id,
        size: usize::try_from(size).ok()?,
        format_flags,
    })
}

/// Size of the ID3v2.4 frame at `pos`, choosing between the synchsafe and the
/// plain reading of `bytes` (see the module documentation).
fn frame_size_v24(data: &[u8], pos: usize, bytes: [u8; 4]) -> Option<u32> {
    let plain = u32::from_be_bytes(bytes);
    let Some(safe) = synchsafe(bytes) else {
        return Some(plain);
    };
    if safe == plain {
        return Some(safe);
    }
    let ends_well = |size: u32| {
        let next = usize::try_from(size)
            .ok()
            .and_then(|s| (pos + 10).checked_add(s));
        next.is_some_and(|next| boundary(data, next))
    };
    if ends_well(safe) {
        Some(safe)
    } else if ends_well(plain) {
        Some(plain)
    } else {
        None
    }
}

/// Returns `true` if a frame may end at `next`: the end of the frames,
/// padding, or a frame header with a valid identifier whose size fits.
fn boundary(data: &[u8], next: usize) -> bool {
    if next == data.len() {
        return true;
    }
    match data.get(next..) {
        Some([0, ..]) => true,
        Some(rest) => rest.first_chunk::<10>().is_some_and(|header| {
            let [a, b, c, d, s0, s1, s2, s3, ..] = *header;
            let bytes = [s0, s1, s2, s3];
            let size = synchsafe(bytes).unwrap_or(u32::from_be_bytes(bytes));
            valid_id(&[a, b, c, d])
                && usize::try_from(size).is_ok_and(|size| size <= rest.len() - 10)
        }),
        None => false,
    }
}

/// Returns `true` for an identifier of `A..=Z` and `0..=9`, or a
/// three-character one followed by `0x00` (an unmapped ID3v2.2 identifier).
fn valid_id(id: &[u8; 4]) -> bool {
    let valid = |b: &u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    match id {
        [a, b, c, 0] => [a, b, c].into_iter().all(valid),
        _ => id.iter().all(valid),
    }
}

/// Removes the optional fields of the frame header and the frame
/// unsynchronisation from `content`. Returns the payload and whether it is
/// compressed or encrypted, or `None` if the optional fields do not fit.
fn frame_payload(
    content: &[u8],
    flags: u8,
    format: Format,
) -> Option<(std::borrow::Cow<'_, [u8]>, bool)> {
    use std::borrow::Cow;
    let (extra, opaque, unsync) = match format.major {
        2 => (0, false, false),
        3 => {
            let compressed = flags & 0x80 != 0;
            let encrypted = flags & 0x40 != 0;
            let grouped = flags & 0x20 != 0;
            let extra = 4 * usize::from(compressed) + usize::from(encrypted) + usize::from(grouped);
            (extra, compressed || encrypted, false)
        }
        _ => {
            let grouped = flags & 0x40 != 0;
            let compressed = flags & 0x08 != 0;
            let encrypted = flags & 0x04 != 0;
            let unsync = flags & 0x02 != 0 || format.unsync_frames;
            let length = flags & 0x01 != 0;
            let extra = usize::from(grouped) + usize::from(encrypted) + 4 * usize::from(length);
            (extra, compressed || encrypted, unsync)
        }
    };
    let payload = content.get(extra..)?;
    if unsync {
        let mut owned = payload.to_vec();
        remove_unsync(&mut owned);
        Some((Cow::Owned(owned), opaque))
    } else {
        Some((Cow::Borrowed(payload), opaque))
    }
}

#[cfg(test)]
mod tests;
