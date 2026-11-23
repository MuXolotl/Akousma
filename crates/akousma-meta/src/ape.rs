//! APEv1 and APEv2 tags.
//!
//! An APE tag is a list of items framed by 32-byte header and footer
//! structures ([`ApeHeader`]). Every number is a little-endian `u32`:
//!
//! | Bytes | Field |
//! | :--- | :--- |
//! | `0..8` | `"APETAGEX"` |
//! | `8..12` | version: `1000` (APEv1) or `2000` (APEv2) |
//! | `12..16` | tag size: the items plus the footer, never the header |
//! | `16..20` | item count |
//! | `20..24` | flags (APEv2 only, ignored in APEv1): bit 31 set if the tag has a header, bit 30 set if the tag has **no** footer, bit 29 set if this structure is the header |
//! | `24..32` | reserved |
//!
//! APEv1 tags have a footer and no header. APEv2 tags at the end of a file
//! have a footer and may have a header; APEv2 tags at the start of a file
//! have a header and may have a footer. [`ApeHeader::items_len`] and
//! [`ApeHeader::total_len`] apply these rules. Writers disagree on bit 30,
//! so [`read_leading`] finds the footer of a tag at the start of a file by
//! its [`MAGIC`] instead.
//!
//! Each item is `value size: u32`, `flags: u32`, a key of 2 to 255 ASCII
//! characters `0x20..=0x7E` followed by `0x00`, and the value. Bits 1 and 2
//! of the item flags give the type of the value (APEv1 values are always
//! text; APEv1 text that is not valid UTF-8 is read as Windows-1252, as
//! APEv1 writers stored text in the code page of the system):
//!
//! | Type | Value | Raw value | Normalized |
//! | :--- | :--- | :--- | :--- |
//! | `0` | UTF-8 text, several values separated by `0x00` | text, or a text list | from the first value, by key (below) |
//! | `1` | binary | binary, or a [`Visual`] for `Cover Art` keys | none |
//! | `2` | UTF-8 locator (URL or file name) | text, or a [`Visual`] with media type `"-->"` for `Cover Art` keys | none |
//! | `3` | reserved | binary | none |
//!
//! Keys are matched without regard to ASCII case:
//!
//! | Key | Normalized |
//! | :--- | :--- |
//! | `Title`, `Subtitle`, `Artist`, `Album`, `Composer`, `Conductor`, `Publisher`, `Genre`, `Comment`, `Copyright`, `Language` | the [`StandardTag`] of the same meaning |
//! | `ISRC`, `EAN/UPC`, `Catalog` | [`Isrc`](StandardTag::Isrc), [`Barcode`](StandardTag::Barcode), [`CatalogNumber`](StandardTag::CatalogNumber) |
//! | `Track` (`n` or `n/total`) | [`TrackNumber`](StandardTag::TrackNumber) |
//! | `Year`, `Record Date` | [`ReleaseDate`](StandardTag::ReleaseDate), [`RecordingDate`](StandardTag::RecordingDate), if the value matches a supported date form |
//!
//! A binary `Cover Art (Front)` item holds a file name or description, a
//! `0x00` byte and the image. The keys `Cover Art (Other)`,
//! `Cover Art (Png Icon)`, `Cover Art (Icon)`, `Cover Art (Front)`,
//! `Cover Art (Back)`, `Cover Art (Leaflet)`, `Cover Art (Media)`,
//! `Cover Art (Lead Artist)`, `Cover Art (Artist)`, `Cover Art (Conductor)`,
//! `Cover Art (Band)`, `Cover Art (Composer)`, `Cover Art (Lyricist)`,
//! `Cover Art (Recording Location)`, `Cover Art (During Recording)`,
//! `Cover Art (During Performance)`, `Cover Art (Video Capture)`,
//! `Cover Art (Fish)`, `Cover Art (Illustration)`,
//! `Cover Art (Band Logotype)` and `Cover Art (Publisher Logotype)` give the
//! picture types `0..=20` in this order; other `Cover Art` keys give
//! [`VisualKind::Other`]. The media type comes from the first bytes of the
//! image (PNG, JPEG, GIF, WebP, BMP, TIFF) and is empty if they match none.

use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};

use akousma_core::io::ByteReader;
use akousma_core::meta::{
    MetadataBudget, MetadataRevision, RawValue, StandardTag, Visual, VisualKind,
};
use akousma_core::{Error, ErrorKind};

use crate::encoding::{split_nul, utf8_or_windows1252};
use crate::stream::{BUFFER_LEN, TagStream, Utf8Check};
use crate::{
    Found, Items, LeadingTag, MAX_FIELD_VALUES, MAX_REVISION_FIELDS, TextTag, TrailingTag,
    check_room, iso_date, new_tag, parse_position, parsed, split_values,
};

/// First bytes of an APE tag header or footer.
pub const MAGIC: [u8; 8] = *b"APETAGEX";

/// Length of an APE tag header or footer in bytes.
pub const HEADER_LEN: usize = 32;

/// Version number of APEv1 tags.
pub const VERSION_1: u32 = 1000;

/// Version number of APEv2 tags.
pub const VERSION_2: u32 = 2000;

/// Header and footer length as a `u32`, for tag size arithmetic.
const HEADER_LEN_U32: u32 = HEADER_LEN as u32;

/// Tag flag: the tag has a header.
const FLAG_HAS_HEADER: u32 = 1 << 31;

/// Tag flag: the tag has no footer.
const FLAG_HAS_NO_FOOTER: u32 = 1 << 30;

/// Tag flag: this structure is the header.
const FLAG_IS_HEADER: u32 = 1 << 29;

/// Item types, stored in bits 1 and 2 of the item flags.
const ITEM_TEXT: u32 = 0;
const ITEM_BINARY: u32 = 1;
const ITEM_LOCATOR: u32 = 2;

/// Length of the value size and flags that start an item.
const ITEM_HEADER_LEN: usize = 8;

/// Shortest and longest item key.
const MIN_KEY_LEN: usize = 2;
const MAX_KEY_LEN: usize = 255;

/// Keys that items must not use.
const FORBIDDEN_KEYS: [&str; 4] = ["ID3", "TAG", "OggS", "MP+"];

/// Prefix of the keys of picture items.
const COVER_ART: &str = "Cover Art";

/// Picture item keys, indexed by picture type.
const COVER_ART_KEYS: [&str; 21] = [
    "Cover Art (Other)",
    "Cover Art (Png Icon)",
    "Cover Art (Icon)",
    "Cover Art (Front)",
    "Cover Art (Back)",
    "Cover Art (Leaflet)",
    "Cover Art (Media)",
    "Cover Art (Lead Artist)",
    "Cover Art (Artist)",
    "Cover Art (Conductor)",
    "Cover Art (Band)",
    "Cover Art (Composer)",
    "Cover Art (Lyricist)",
    "Cover Art (Recording Location)",
    "Cover Art (During Recording)",
    "Cover Art (During Performance)",
    "Cover Art (Video Capture)",
    "Cover Art (Fish)",
    "Cover Art (Illustration)",
    "Cover Art (Band Logotype)",
    "Cover Art (Publisher Logotype)",
];

/// Text item keys and the normalized meaning of their value.
const TEXT_KEYS: [(&str, TextTag); 14] = [
    ("Title", StandardTag::TrackTitle),
    ("Subtitle", StandardTag::TrackSubtitle),
    ("Artist", StandardTag::Artist),
    ("Album", StandardTag::Album),
    ("Composer", StandardTag::Composer),
    ("Conductor", StandardTag::Conductor),
    ("Publisher", StandardTag::Publisher),
    ("Genre", StandardTag::Genre),
    ("Comment", StandardTag::Comment),
    ("Copyright", StandardTag::Copyright),
    ("Language", StandardTag::Language),
    ("ISRC", StandardTag::Isrc),
    ("EAN/UPC", StandardTag::Barcode),
    ("Catalog", StandardTag::CatalogNumber),
];

/// A parsed APE tag header or footer.
///
/// ```
/// use akousma_meta::ape::ApeHeader;
///
/// // APEv2 footer of a tag with a header: 100 bytes of items plus the footer.
/// let mut footer = [0u8; 32];
/// footer[..8].copy_from_slice(b"APETAGEX");
/// footer[8..12].copy_from_slice(&2000u32.to_le_bytes());
/// footer[12..16].copy_from_slice(&132u32.to_le_bytes());
/// footer[16..20].copy_from_slice(&3u32.to_le_bytes());
/// footer[20..24].copy_from_slice(&(1u32 << 31).to_le_bytes());
///
/// let footer = ApeHeader::parse(&footer)?;
/// assert!(!footer.is_header() && footer.has_header() && footer.has_footer());
/// assert_eq!(footer.items_len(), 100);
/// assert_eq!(footer.total_len(), 164);
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApeHeader {
    /// [`VERSION_1`] or [`VERSION_2`].
    pub version: u32,
    /// Size of the items plus the footer, if the tag has one.
    pub tag_size: u32,
    /// Number of items.
    pub item_count: u32,
    /// Tag flags; always `0` for APEv1, whose flags are ignored.
    pub flags: u32,
}

impl ApeHeader {
    /// Parses a 32-byte header or footer.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidHeader`] at offset 0 if `bytes` do not start with
    /// [`MAGIC`], and [`ErrorKind::UnsupportedFeature`] at offset 8 if the
    /// version is neither [`VERSION_1`] nor [`VERSION_2`].
    pub fn parse(bytes: &[u8; HEADER_LEN]) -> Result<Self, Error> {
        let mut reader = ByteReader::new(&bytes[..]);
        if reader.read_array::<8>()? != MAGIC {
            return Err(Error::at(ErrorKind::InvalidHeader, 0));
        }
        let version = reader.read_u32_le()?;
        if version != VERSION_1 && version != VERSION_2 {
            return Err(Error::at(ErrorKind::UnsupportedFeature, 8));
        }
        let tag_size = reader.read_u32_le()?;
        let item_count = reader.read_u32_le()?;
        let flags = reader.read_u32_le()?;
        Ok(Self {
            version,
            tag_size,
            item_count,
            flags: if version == VERSION_1 { 0 } else { flags },
        })
    }

    /// Returns `true` if this structure is the header of its tag, `false` if
    /// it is the footer. Always `false` for APEv1.
    #[must_use]
    pub const fn is_header(&self) -> bool {
        self.flags & FLAG_IS_HEADER != 0
    }

    /// Returns `true` if the tag has a header. Always `false` for APEv1,
    /// which has no header whatever its flags say.
    #[must_use]
    pub const fn has_header(&self) -> bool {
        self.flags & FLAG_HAS_HEADER != 0
    }

    /// Returns `true` if the tag has a footer (flag bit 30 clear). Always
    /// `true` for APEv1.
    #[must_use]
    pub const fn has_footer(&self) -> bool {
        self.flags & FLAG_HAS_NO_FOOTER == 0
    }

    /// Number of bytes of the items.
    ///
    /// For a footer, the tag size includes the footer; a tag size below 32 is
    /// read as 32 (no items). For a header, the tag size includes the footer
    /// only if the tag has one, as flag bit 30 says; some writers set that
    /// bit wrongly, which [`read_leading`] avoids by looking for the footer.
    #[must_use]
    pub const fn items_len(&self) -> u32 {
        if !self.is_header() {
            // `max` is not `const` for integers before Rust 1.95.
            let size = if self.tag_size < HEADER_LEN_U32 {
                HEADER_LEN_U32
            } else {
                self.tag_size
            };
            size - HEADER_LEN_U32
        } else if self.has_footer() {
            self.tag_size.saturating_sub(HEADER_LEN_U32)
        } else {
            self.tag_size
        }
    }

    /// Number of bytes of the whole tag: header (if any), items and footer
    /// (if any).
    ///
    /// For a footer, this is the tag size (at least 32) plus 32 if the tag has
    /// a header. For a header, this is 32 plus the tag size.
    #[must_use]
    pub const fn total_len(&self) -> u64 {
        let header = HEADER_LEN as u64;
        if self.is_header() {
            header + self.tag_size as u64
        } else if self.has_header() {
            header + self.items_len() as u64 + header
        } else {
            self.items_len() as u64 + header
        }
    }
}

/// Parses the items of an APE tag.
///
/// `items` holds the [`ApeHeader::items_len`] bytes between the header and
/// the footer. Reading stops after [`ApeHeader::item_count`] items, or
/// earlier if the last item ends exactly at the end of `items`: some writers
/// store a count larger than the number of items.
///
/// ```
/// use akousma_core::meta::{RawValue, StandardTag};
/// use akousma_meta::ape::{self, ApeHeader};
///
/// let mut items = Vec::new();
/// items.extend_from_slice(&15u32.to_le_bytes());
/// items.extend_from_slice(&0u32.to_le_bytes()); // UTF-8 text
/// items.extend_from_slice(b"Artist\0Artist1\0Artist2");
///
/// let header = ApeHeader { version: 2000, tag_size: 55, item_count: 1, flags: 0 };
/// let revision = ape::parse_items(&header, &items)?;
/// let tag = &revision.tags[0];
/// assert_eq!(tag.raw.key, "Artist");
/// assert_eq!(tag.raw.value, RawValue::MultiText(vec!["Artist1".into(), "Artist2".into()]));
/// assert_eq!(tag.standard, Some(StandardTag::Artist("Artist1".into())));
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// - [`ErrorKind::UnexpectedEof`] if an item does not fit in `items`.
/// - [`ErrorKind::InvalidData`] for a key that is too short, too long,
///   forbidden or not printable ASCII, an APEv2 text value that is not UTF-8,
///   a `Cover Art` value without the `0x00` after its description, or bytes
///   left over after the last item.
/// - [`ErrorKind::LimitExceeded`] for more than 65 536 items, or a value
///   with more than 65 536 `0x00`-separated parts.
pub fn parse_items(header: &ApeHeader, items: &[u8]) -> Result<MetadataRevision, Error> {
    parse_items_at(header, items, 0)
}

/// Reads an APE tag whose last byte is at `end - 1`, such as the end of a
/// file or the byte before an ID3v1 tag, and that starts at or after
/// `floor`.
///
/// Returns `Ok(None)` if the 32 bytes before `end` are not an APE footer.
/// Once the footer and the header it announces are checked, the stored size
/// of the whole tag (header, items and footer) is taken from the byte limit
/// of `budget`, then the items are read and parsed. Entries are not taken:
/// that is up to the caller, which decides whether the revision is kept.
///
/// ```
/// use akousma_core::io::ByteReader;
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::FormatLimits;
/// use akousma_meta::ape;
/// use std::io::Cursor;
///
/// let mut file = b"audio".to_vec();
/// file.extend_from_slice(&5u32.to_le_bytes());
/// file.extend_from_slice(&0u32.to_le_bytes());
/// file.extend_from_slice(b"Title\0Intro");
/// file.extend_from_slice(b"APETAGEX");
/// for field in [1000u32, 19 + 32, 1, 0, 0, 0] {
///     file.extend_from_slice(&field.to_le_bytes());
/// }
///
/// let end = file.len() as u64;
/// let mut budget = MetadataBudget::new(&FormatLimits::new().with_max_metadata_bytes(100));
/// let mut reader = ByteReader::new(Cursor::new(file));
/// let tag = ape::read_trailing(&mut reader, end, 0, &mut budget)?.expect("APE tag");
/// assert_eq!(tag.start, 5);
/// assert_eq!(tag.revision.tags[0].raw.key, "Title");
/// assert_eq!(budget.remaining_bytes(), 100 - 51);
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// - [`ErrorKind::InvalidHeader`] if the footer is a header, if the tag would
///   start before `floor` or if its header lacks the [`MAGIC`].
/// - [`ErrorKind::LimitExceeded`] at the start of the tag if it is larger
///   than what is left of `budget`, which is then unchanged.
/// - The errors of [`ApeHeader::parse`] and [`parse_items`], with absolute
///   offsets, and the errors of the reader.
///
/// Both an item that runs past the end of the tag and a source that ends
/// inside the tag are [`ErrorKind::UnexpectedEof`] here; the scanner of
/// [`anchors`](crate::anchors) tells them apart and fails only on the
/// second.
pub fn read_trailing<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
    budget: &mut MetadataBudget,
) -> Result<Option<TrailingTag>, Error> {
    find_trailing(reader, end, floor, budget, Items::Parse)?.into_result()
}

/// [`read_trailing`] that tells a damaged tag from an error of the source.
/// With [`Items::Check`] the revision is empty.
pub(crate) fn find_trailing<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
    budget: &mut MetadataBudget,
    items: Items,
) -> Result<Found<TrailingTag>, Error> {
    Ok(match trailing_frame(reader, end, floor)? {
        Found::Tag(frame) => {
            frame_items(reader, &frame, budget, items)?.map(|revision| TrailingTag {
                start: frame.start,
                revision,
            })
        }
        Found::Absent => Found::Absent,
        Found::Damaged(err) => Found::Damaged(err),
    })
}

/// Where an APE tag lies in a source, as its header and footer say.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Frame {
    /// First byte of the tag, header included.
    pub(crate) start: u64,
    /// One past the last byte of the tag, footer included.
    pub(crate) end: u64,
    /// The header or footer that gives the item count and the version.
    pub(crate) header: ApeHeader,
    /// First byte of the items.
    pub(crate) items_start: u64,
    /// Number of bytes of the items.
    pub(crate) items_len: u64,
}

/// Finds the frame of the APE tag whose footer ends at `end`, if the tag
/// starts at or after `floor`.
pub(crate) fn trailing_frame<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
) -> Result<Found<Frame>, Error> {
    let Some(footer_pos) = end.checked_sub(HEADER_LEN as u64) else {
        return Ok(Found::Absent);
    };
    reader.seek(SeekFrom::Start(footer_pos))?;
    let footer = reader.read_array::<HEADER_LEN>()?;
    if !footer.starts_with(&MAGIC) {
        return Ok(Found::Absent);
    }
    let header = match ApeHeader::parse(&footer) {
        Ok(header) => header,
        Err(err) => return Ok(Found::Damaged(shift(err, footer_pos))),
    };
    let invalid = Found::Damaged(Error::at(ErrorKind::InvalidHeader, footer_pos));
    if header.is_header() {
        return Ok(invalid);
    }
    let Some(start) = end
        .checked_sub(header.total_len())
        .filter(|&start| start >= floor)
    else {
        return Ok(invalid);
    };
    let items_len = u64::from(header.items_len());
    if header.has_header() {
        reader.seek(SeekFrom::Start(start))?;
        if !reader.read_array::<HEADER_LEN>()?.starts_with(&MAGIC) {
            return Ok(Found::Damaged(Error::at(ErrorKind::InvalidHeader, start)));
        }
    }
    Ok(Found::Tag(Frame {
        start,
        end,
        header,
        items_start: footer_pos - items_len,
        items_len,
    }))
}

/// Reads and parses the items of the tag of `frame` ([`read_frame`]), or
/// checks them ([`check_frame`]) and returns an empty revision.
fn frame_items<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    frame: &Frame,
    budget: &mut MetadataBudget,
    items: Items,
) -> Result<Found<MetadataRevision>, Error> {
    Ok(match items {
        Items::Parse => read_frame(reader, frame, budget)?,
        Items::Check => check_frame(reader, frame, budget, &mut [0; BUFFER_LEN])?
            .map(|()| MetadataRevision::default()),
    })
}

/// Takes the stored size of the tag of `frame` from the byte limit of
/// `budget`, then reads and parses its items.
pub(crate) fn read_frame<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    frame: &Frame,
    budget: &mut MetadataBudget,
) -> Result<Found<MetadataRevision>, Error> {
    budget
        .take_bytes(frame.end - frame.start)
        .map_err(|kind| Error::at(kind, frame.start))?;
    reader.seek(SeekFrom::Start(frame.items_start))?;
    let items = reader.read_bounded_vec(frame.items_len, usize::MAX)?;
    parsed(parse_items_at(&frame.header, &items, frame.items_start))
}

/// Checks the items of the tag of `frame` by the rules of [`parse_items`],
/// reading them through `buf` (which must not be empty) and keeping
/// nothing. The result is that of [`read_frame`], errors and offsets
/// included, except that the byte limit of `budget` is not used: every read
/// is taken from its scanned bytes instead, and errors of the source are
/// those of the bytes actually read. The bytes that no rule looks at
/// (binary values, the image of a picture) are skipped by seeking.
pub(crate) fn check_frame<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    frame: &Frame,
    budget: &mut MetadataBudget,
    buf: &mut [u8],
) -> Result<Found<()>, Error> {
    let end = frame.items_start.saturating_add(frame.items_len);
    let mut items = TagStream::new(reader, budget, frame.start, frame.items_start, end, buf);
    check_items(&frame.header, &mut items)
}

/// The checks of [`parse_items_at`] on the items of `stream`, in the same
/// order. Rules that a tag breaks give [`Found::Damaged`]; exhausted limits
/// are errors, as in [`parsed`].
fn check_items<R: Read + Seek>(
    header: &ApeHeader,
    items: &mut TagStream<'_, R>,
) -> Result<Found<()>, Error> {
    let damaged = |kind, at| Ok(Found::Damaged(Error::at(kind, at)));
    let apev1 = header.version == VERSION_1;
    let mut fields = 0usize;
    for _ in 0..header.item_count {
        if items.remaining() == 0 {
            break;
        }
        let item_start = items.position();
        if items.remaining() < ITEM_HEADER_LEN as u64 {
            return damaged(ErrorKind::UnexpectedEof, item_start);
        }
        let mut fixed = [0; ITEM_HEADER_LEN];
        items.read_exact(&mut fixed)?;
        let [l0, l1, l2, l3, f0, f1, f2, f3] = fixed;
        let value_len = u32::from_le_bytes([l0, l1, l2, l3]);
        let flags = u32::from_le_bytes([f0, f1, f2, f3]);

        // The key and its 0x00, as far as `read_key` would look.
        let key_start = items.position();
        let mut window = [0; MAX_KEY_LEN + 1];
        let mut window_len = 0;
        while window_len < window.len() {
            let Some(&byte) = items.peek(1)?.first() else {
                break;
            };
            items.consume(1);
            window[window_len] = byte;
            window_len += 1;
            if byte == 0 {
                break;
            }
        }
        let key = match read_key(&window[..window_len]) {
            Ok(key) => key,
            Err(kind) => return damaged(kind, key_start),
        };

        let value_start = items.position();
        if u64::from(value_len) > items.remaining() {
            return damaged(ErrorKind::UnexpectedEof, value_start);
        }
        if fields >= MAX_REVISION_FIELDS {
            return Err(Error::at(ErrorKind::LimitExceeded, value_start));
        }
        let item_type = if apev1 { ITEM_TEXT } else { (flags >> 1) & 3 };
        let value = check_value(items, key, item_type, apev1, u64::from(value_len))?;
        match value {
            Ok(()) => fields += 1,
            Err(kind) if kind == ErrorKind::LimitExceeded => {
                return Err(Error::at(kind, value_start));
            }
            Err(kind) => return damaged(kind, value_start),
        }
    }
    if items.remaining() != 0 {
        return damaged(ErrorKind::InvalidData, items.position());
    }
    Ok(Found::Tag(()))
}

/// The checks of [`add_item`] on the next `len` bytes of `items`, all of
/// which are used up. The inner error is that of [`add_item`].
fn check_value<R: Read + Seek>(
    items: &mut TagStream<'_, R>,
    key: &str,
    item_type: u32,
    apev1: bool,
    len: u64,
) -> Result<Result<(), ErrorKind>, Error> {
    let cover_art = is_cover_art(key);
    let mut text = Utf8Check::new();
    let mut left = len;
    Ok(match item_type {
        ITEM_BINARY if cover_art => {
            // The description, up to the 0x00 before the image.
            let mut terminated = false;
            while left > 0 && !terminated {
                let chunk = items.peek(left)?;
                let used = match chunk.iter().position(|&b| b == 0) {
                    Some(nul) => {
                        terminated = true;
                        text.push(&chunk[..nul]);
                        nul + 1
                    }
                    None => {
                        text.push(chunk);
                        chunk.len()
                    }
                };
                items.consume(used);
                left -= used as u64;
            }
            items.skip(left);
            if terminated && text.is_valid() {
                Ok(())
            } else {
                Err(ErrorKind::InvalidData)
            }
        }
        ITEM_TEXT | ITEM_LOCATOR => {
            // APEv1 text that is not UTF-8 is Windows-1252, where 0x00 is
            // U+0000 as in UTF-8: the values are counted the same way.
            let mut separators = 0usize;
            while left > 0 {
                let chunk = items.peek(left)?;
                text.push(chunk);
                separators = separators.saturating_add(chunk.iter().filter(|&&b| b == 0).count());
                let used = chunk.len();
                items.consume(used);
                left -= used as u64;
            }
            let split = item_type == ITEM_TEXT || !cover_art;
            if !(apev1 || text.is_valid()) {
                Err(ErrorKind::InvalidData)
            } else if split && separators >= MAX_FIELD_VALUES {
                Err(ErrorKind::LimitExceeded)
            } else {
                Ok(())
            }
        }
        _ => {
            items.skip(left);
            Ok(())
        }
    })
}

/// Returns `true` for the keys of picture items.
fn is_cover_art(key: &str) -> bool {
    key.get(..COVER_ART.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(COVER_ART))
}

/// Reads an APEv2 tag that starts with a header at `start`, such as the
/// start of a file, and ends at or before `limit`.
///
/// Returns `Ok(None)` if the bytes at `start` do not start with [`MAGIC`].
/// Whether the tag has a footer is decided by looking for [`MAGIC`] in its
/// last 32 bytes, not by flag bit 30, which writers set inconsistently.
/// Then the stored size of the whole tag is taken from the byte limit of
/// `budget`, and the items are read and parsed. Entries are not taken.
///
/// ```
/// use akousma_core::io::ByteReader;
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::FormatLimits;
/// use akousma_meta::ape;
/// use std::io::Cursor;
///
/// let mut item = 5u32.to_le_bytes().to_vec();
/// item.extend_from_slice(&0u32.to_le_bytes());
/// item.extend_from_slice(b"Title\0Intro");
/// let structure = |flags: u32| {
///     let mut bytes = b"APETAGEX".to_vec();
///     for field in [2000u32, 19 + 32, 1, flags, 0, 0] {
///         bytes.extend_from_slice(&field.to_le_bytes());
///     }
///     bytes
/// };
/// // Header and footer, with flag bit 30 set as some writers do.
/// let mut file = structure(0xE000_0000);
/// file.extend_from_slice(&item);
/// file.extend_from_slice(&structure(0xC000_0000));
/// file.extend_from_slice(b"audio");
///
/// let limit = file.len() as u64;
/// let mut budget = MetadataBudget::new(&FormatLimits::new());
/// let mut reader = ByteReader::new(Cursor::new(file));
/// let tag = ape::read_leading(&mut reader, 0, limit, &mut budget)?.expect("APE tag");
/// assert_eq!(tag.end, 32 + 19 + 32);
/// assert_eq!(tag.revision.tags[0].raw.key, "Title");
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// - [`ErrorKind::InvalidHeader`] if the structure at `start` is not the
///   header of an APEv2 tag, or the tag ends after `limit`.
/// - [`ErrorKind::LimitExceeded`] at `start` if the tag is larger than what
///   is left of `budget`, which is then unchanged.
/// - The errors of [`ApeHeader::parse`] and [`parse_items`], with absolute
///   offsets, and the errors of the reader, such as
///   [`ErrorKind::UnexpectedEof`] for a tag cut by the end of the source.
///
/// Both an item that runs past the end of the tag and a source that ends
/// inside the tag are [`ErrorKind::UnexpectedEof`] here; the scanner of
/// [`anchors`](crate::anchors) tells them apart and fails only on the
/// second.
pub fn read_leading<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    start: u64,
    limit: u64,
    budget: &mut MetadataBudget,
) -> Result<Option<LeadingTag>, Error> {
    find_leading(reader, start, limit, budget, Items::Parse)?.into_result()
}

/// [`read_leading`] that tells a damaged tag from an error of the source.
/// With [`Items::Check`] the revision is empty.
pub(crate) fn find_leading<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    start: u64,
    limit: u64,
    budget: &mut MetadataBudget,
    items: Items,
) -> Result<Found<LeadingTag>, Error> {
    Ok(match leading_frame(reader, start, limit)? {
        Found::Tag(frame) => {
            frame_items(reader, &frame, budget, items)?.map(|revision| LeadingTag {
                end: frame.end,
                revision,
            })
        }
        Found::Absent => Found::Absent,
        Found::Damaged(err) => Found::Damaged(err),
    })
}

/// Finds the frame of the APEv2 tag whose header is at `start`, if the tag
/// ends at or before `limit`.
pub(crate) fn leading_frame<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    start: u64,
    limit: u64,
) -> Result<Found<Frame>, Error> {
    reader.seek(SeekFrom::Start(start))?;
    let bytes = reader.read_array::<HEADER_LEN>()?;
    if !bytes.starts_with(&MAGIC) {
        return Ok(Found::Absent);
    }
    let header = match ApeHeader::parse(&bytes) {
        Ok(header) => header,
        Err(err) => return Ok(Found::Damaged(shift(err, start))),
    };
    let invalid = Found::Damaged(Error::at(ErrorKind::InvalidHeader, start));
    if header.version != VERSION_2 || !header.is_header() {
        return Ok(invalid);
    }
    let items_start = start + HEADER_LEN as u64;
    let Some(end) = items_start
        .checked_add(u64::from(header.tag_size))
        .filter(|&end| end <= limit)
    else {
        return Ok(invalid);
    };
    let mut items_len = u64::from(header.tag_size);
    if let Some(footer_pos) = end
        .checked_sub(HEADER_LEN as u64)
        .filter(|&p| p >= items_start)
    {
        reader.seek(SeekFrom::Start(footer_pos))?;
        if reader.read_array::<HEADER_LEN>()?.starts_with(&MAGIC) {
            items_len -= HEADER_LEN as u64;
        }
    }
    Ok(Found::Tag(Frame {
        start,
        end,
        header,
        items_start,
        items_len,
    }))
}

/// Moves the offset of `err` by `base` bytes.
fn shift(err: Error, base: u64) -> Error {
    match err.byte_offset {
        Some(offset) => Error::at(err.kind, offset.saturating_add(base)),
        None => err,
    }
}

/// Parses APE items; `base` is the absolute offset of `items[0]`.
pub(crate) fn parse_items_at(
    header: &ApeHeader,
    items: &[u8],
    base: u64,
) -> Result<MetadataRevision, Error> {
    let at = |pos: usize| base.saturating_add(pos as u64);
    let mut revision = MetadataRevision::default();
    let mut pos = 0;
    for _ in 0..header.item_count {
        // Some writers store an item count larger than the number of items:
        // items that end exactly at the end of the data are accepted.
        if pos == items.len() {
            break;
        }
        let fixed = items
            .get(pos..pos + ITEM_HEADER_LEN)
            .ok_or(Error::at(ErrorKind::UnexpectedEof, at(pos)))?;
        let mut fields = ByteReader::with_offset(fixed, at(pos));
        let value_len = fields.read_u32_le()?;
        let flags = fields.read_u32_le()?;
        pos += ITEM_HEADER_LEN;

        let key_start = pos;
        let key = read_key(&items[pos..]).map_err(|kind| Error::at(kind, at(key_start)))?;
        pos += key.len() + 1;

        let value_start = pos;
        let value = usize::try_from(value_len)
            .ok()
            .and_then(|len| items.get(pos..pos.checked_add(len)?))
            .ok_or(Error::at(ErrorKind::UnexpectedEof, at(value_start)))?;
        pos += value.len();

        // APEv1 has no item flags: every value is text.
        let apev1 = header.version == VERSION_1;
        let item_type = if apev1 { ITEM_TEXT } else { (flags >> 1) & 3 };
        add_item(&mut revision, key, item_type, apev1, value)
            .map_err(|kind| Error::at(kind, at(value_start)))?;
    }
    if pos != items.len() {
        return Err(Error::at(ErrorKind::InvalidData, at(pos)));
    }
    Ok(revision)
}

/// Reads the `0x00`-terminated key at the start of `bytes`.
fn read_key(bytes: &[u8]) -> Result<&str, ErrorKind> {
    let window = &bytes[..bytes.len().min(MAX_KEY_LEN + 1)];
    let Some(len) = window.iter().position(|&b| b == 0) else {
        return Err(if window.len() > MAX_KEY_LEN {
            ErrorKind::InvalidData
        } else {
            ErrorKind::UnexpectedEof
        });
    };
    let key = &bytes[..len];
    if len < MIN_KEY_LEN || !key.iter().all(|b| (0x20..=0x7E).contains(b)) {
        return Err(ErrorKind::InvalidData);
    }
    let key = core::str::from_utf8(key).map_err(|_| ErrorKind::InvalidData)?;
    if FORBIDDEN_KEYS.contains(&key) {
        return Err(ErrorKind::InvalidData);
    }
    Ok(key)
}

/// Adds one item to `revision`. `apev1` selects the APEv1 reading of text:
/// UTF-8, or Windows-1252 if not valid UTF-8.
fn add_item(
    revision: &mut MetadataRevision,
    key: &str,
    item_type: u32,
    apev1: bool,
    value: &[u8],
) -> Result<(), ErrorKind> {
    check_room(revision)?;
    let cover_art = is_cover_art(key);
    match item_type {
        ITEM_TEXT => {
            let text = if apev1 {
                utf8_or_windows1252(value)
            } else {
                Cow::Borrowed(utf8(value)?)
            };
            let first = text.split_once('\0').map_or(&*text, |(first, _)| first);
            let standard = standard_tag(key, first);
            revision
                .tags
                .push(new_tag(key, split_values(&text, '\0')?, standard));
        }
        ITEM_BINARY if cover_art => revision.visuals.push(cover_art_visual(key, value)?),
        ITEM_LOCATOR if cover_art => {
            utf8(value)?;
            revision.visuals.push(Visual {
                kind: cover_art_kind(key),
                media_type: "-->".to_owned(),
                description: String::new(),
                width: None,
                height: None,
                color_depth: None,
                indexed_colors: None,
                data: value.to_vec(),
            });
        }
        ITEM_LOCATOR => revision
            .tags
            .push(new_tag(key, split_values(utf8(value)?, '\0')?, None)),
        _ => revision
            .tags
            .push(new_tag(key, RawValue::Binary(value.to_vec()), None)),
    }
    Ok(())
}

/// Decodes UTF-8 item text.
fn utf8(value: &[u8]) -> Result<&str, ErrorKind> {
    core::str::from_utf8(value).map_err(|_| ErrorKind::InvalidData)
}

/// Normalized meaning of the first value of a text item.
fn standard_tag(key: &str, value: &str) -> Option<StandardTag> {
    if value.is_empty() {
        return None;
    }
    if let Some((_, standard)) = TEXT_KEYS
        .iter()
        .find(|(name, _)| key.eq_ignore_ascii_case(name))
    {
        Some(standard(value.to_owned()))
    } else if key.eq_ignore_ascii_case("Track") {
        parse_position(value).map(StandardTag::TrackNumber)
    } else if key.eq_ignore_ascii_case("Year") {
        iso_date(value).map(StandardTag::ReleaseDate)
    } else if key.eq_ignore_ascii_case("Record Date") {
        iso_date(value).map(StandardTag::RecordingDate)
    } else {
        None
    }
}

/// Picture type of a `Cover Art` key.
fn cover_art_kind(key: &str) -> VisualKind {
    COVER_ART_KEYS
        .iter()
        .zip(0u32..)
        .find(|(name, _)| key.eq_ignore_ascii_case(name))
        .map_or(VisualKind::Other, |(_, code)| VisualKind::from_code(code))
}

/// Builds the picture of a binary `Cover Art` item: description, `0x00`,
/// image bytes.
fn cover_art_visual(key: &str, value: &[u8]) -> Result<Visual, ErrorKind> {
    let (description, Some(data)) = split_nul(value) else {
        return Err(ErrorKind::InvalidData);
    };
    Ok(Visual {
        kind: cover_art_kind(key),
        media_type: image_media_type(data).to_owned(),
        description: utf8(description)?.to_owned(),
        width: None,
        height: None,
        color_depth: None,
        indexed_colors: None,
        data: data.to_vec(),
    })
}

/// MIME type from the signature at the start of an image, or `""` if no
/// signature matches. Each pattern only matches images at least as long as
/// the signature.
fn image_media_type(data: &[u8]) -> &'static str {
    match data {
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => "image/png",
        [0xFF, 0xD8, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        [b'B', b'M', ..] => "image/bmp",
        [b'I', b'I', b'*', 0x00, ..] | [b'M', b'M', 0x00, b'*', ..] => "image/tiff",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use akousma_core::FormatLimits;
    use std::io::Cursor;

    const TEXT: u32 = ITEM_TEXT << 1;
    const BINARY: u32 = ITEM_BINARY << 1;
    const LOCATOR: u32 = ITEM_LOCATOR << 1;
    const RESERVED: u32 = 3 << 1;

    fn item(key: &str, flags: u32, value: &[u8]) -> Vec<u8> {
        let mut bytes = (value.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(value);
        bytes
    }

    fn header(version: u32, tag_size: u32, item_count: u32, flags: u32) -> [u8; HEADER_LEN] {
        let mut bytes = [0u8; HEADER_LEN];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&version.to_le_bytes());
        bytes[12..16].copy_from_slice(&tag_size.to_le_bytes());
        bytes[16..20].copy_from_slice(&item_count.to_le_bytes());
        bytes[20..24].copy_from_slice(&flags.to_le_bytes());
        bytes
    }

    fn parsed(version: u32, tag_size: u32, flags: u32) -> ApeHeader {
        ApeHeader::parse(&header(version, tag_size, 0, flags)).unwrap()
    }

    fn v2(item_count: u32, items: &[u8]) -> Result<MetadataRevision, Error> {
        let header = ApeHeader {
            version: VERSION_2,
            tag_size: 0,
            item_count,
            flags: 0,
        };
        parse_items(&header, items)
    }

    fn read(file: &[u8], limits: &FormatLimits) -> Result<Option<TrailingTag>, Error> {
        let mut reader = ByteReader::new(Cursor::new(file));
        let mut budget = MetadataBudget::new(limits);
        read_trailing(&mut reader, file.len() as u64, 0, &mut budget)
    }

    #[test]
    fn apev1_vs_apev2_headers_and_multivalue() {
        // APEv1 with a stray bit 31: no header, flags ignored, values are text.
        let v1 = parsed(
            VERSION_1,
            132,
            FLAG_HAS_HEADER | FLAG_HAS_NO_FOOTER | FLAG_IS_HEADER,
        );
        assert_eq!(v1.flags, 0);
        assert!(!v1.has_header() && v1.has_footer() && !v1.is_header());
        assert_eq!((v1.items_len(), v1.total_len()), (100, 132));

        let items = item("Title", BINARY, b"abc");
        let mut file = vec![0xFFu8; 40];
        file.extend_from_slice(&items);
        file.extend_from_slice(&header(
            VERSION_1,
            items.len() as u32 + 32,
            1,
            FLAG_HAS_HEADER,
        ));
        let tag = read(&file, &FormatLimits::new()).unwrap().unwrap();
        assert_eq!(
            tag.start, 40,
            "no 32-byte header may be subtracted for APEv1"
        );
        assert_eq!(tag.revision.tags[0].raw.value, RawValue::Text("abc".into()));
        assert_eq!(
            tag.revision.tags[0].standard,
            Some(StandardTag::TrackTitle("abc".into()))
        );

        // APEv1 text: UTF-8, or Windows-1252 if not valid UTF-8. APEv2 text
        // must be UTF-8.
        let v1_header = ApeHeader {
            version: VERSION_1,
            tag_size: 0,
            item_count: 2,
            flags: 0,
        };
        let mut items = item("Artist", TEXT, b"Caf\xE9 \x93Ol\xE9\x94\0B");
        items.extend(item("Album", TEXT, "Caf\u{E9}".as_bytes()));
        let revision = parse_items(&v1_header, &items).unwrap();
        assert_eq!(
            revision.tags[0].raw.value,
            RawValue::MultiText(vec![
                "Caf\u{E9} \u{201C}Ol\u{E9}\u{201D}".into(),
                "B".into()
            ])
        );
        assert_eq!(
            revision.tags[0].standard,
            Some(StandardTag::Artist(
                "Caf\u{E9} \u{201C}Ol\u{E9}\u{201D}".into()
            ))
        );
        assert_eq!(
            revision.tags[1].standard,
            Some(StandardTag::Album("Caf\u{E9}".into()))
        );
        assert_eq!(
            v2(1, &item("Artist", TEXT, b"Caf\xE9")).unwrap_err().kind,
            ErrorKind::InvalidData
        );

        // APEv2 header: bit 30 set means there is no footer.
        let with_footer = parsed(VERSION_2, 100, FLAG_IS_HEADER | FLAG_HAS_HEADER);
        assert!(with_footer.is_header() && with_footer.has_footer());
        assert_eq!(
            (with_footer.items_len(), with_footer.total_len()),
            (68, 132)
        );
        let without_footer = parsed(
            VERSION_2,
            100,
            FLAG_IS_HEADER | FLAG_HAS_HEADER | FLAG_HAS_NO_FOOTER,
        );
        assert!(!without_footer.has_footer());
        assert_eq!(
            (without_footer.items_len(), without_footer.total_len()),
            (100, 132)
        );
        let tiny_header = parsed(VERSION_2, 10, FLAG_IS_HEADER | FLAG_HAS_HEADER);
        assert_eq!((tiny_header.items_len(), tiny_header.total_len()), (0, 42));

        // APEv2 footer, with and without a header.
        let footer = parsed(VERSION_2, 100, FLAG_HAS_HEADER);
        assert_eq!((footer.items_len(), footer.total_len()), (68, 132));
        let footer = parsed(VERSION_2, 100, 0);
        assert_eq!((footer.items_len(), footer.total_len()), (68, 100));

        // A tag size below 32 is read as 32.
        for size in [0, 10, 31] {
            let footer = parsed(VERSION_2, size, 0);
            assert_eq!((footer.items_len(), footer.total_len()), (0, 32));
        }
        let mut file = b"audio".to_vec();
        file.extend_from_slice(&header(VERSION_2, 10, 0, 0));
        let tag = read(&file, &FormatLimits::new()).unwrap().unwrap();
        assert_eq!(tag.start, 5);
        assert_eq!(tag.revision, MetadataRevision::default());

        // Multiple values.
        let revision = v2(1, &item("Artist", TEXT, b"Artist1\0Artist2")).unwrap();
        assert_eq!(
            revision.tags[0].raw.value,
            RawValue::MultiText(vec!["Artist1".into(), "Artist2".into()])
        );
        assert_eq!(
            revision.tags[0].standard,
            Some(StandardTag::Artist("Artist1".into()))
        );

        // Pictures shorter than the longest signature.
        let mut items = item("Cover Art (Front)", BINARY, b"a.jpg\0\xFF\xD8");
        items.extend(item("Cover Art (Back)", BINARY, b"\0\x89PNG"));
        items.extend(item("Cover Art (Media)", BINARY, b"empty\0"));
        items.extend(item("cover art (leaflet)", BINARY, b"\0RIFF\x10\0\0\0WEBP"));
        items.extend(item("Cover Art (Custom)", BINARY, b"\0RIFF\x10\0\0\0WEB"));
        items.extend(item(
            "Cover Art (Fish)",
            LOCATOR,
            b"http://example.com/a.png",
        ));
        let revision = v2(6, &items).unwrap();
        assert!(revision.tags.is_empty());
        let visuals: Vec<_> = revision
            .visuals
            .iter()
            .map(|v| {
                (
                    v.kind,
                    v.media_type.as_str(),
                    v.description.as_str(),
                    v.data.len(),
                )
            })
            .collect();
        assert_eq!(
            visuals,
            [
                (VisualKind::FrontCover, "image/jpeg", "a.jpg", 2),
                (VisualKind::BackCover, "", "", 4),
                (VisualKind::Media, "", "empty", 0),
                (VisualKind::Leaflet, "image/webp", "", 12),
                (VisualKind::Other, "", "", 11),
                (VisualKind::BrightColoredFish, "-->", "", 24),
            ]
        );
    }

    #[test]
    fn image_signatures() {
        assert_eq!(image_media_type(b"\x89PNG\r\n\x1A\n"), "image/png");
        assert_eq!(image_media_type(b"\x89PNG\r\n\x1A"), "");
        assert_eq!(image_media_type(b"GIF87a"), "image/gif");
        assert_eq!(image_media_type(b"GIF89a\x01"), "image/gif");
        assert_eq!(image_media_type(b"GIF88a"), "");
        assert_eq!(image_media_type(b"BM"), "image/bmp");
        assert_eq!(image_media_type(b"II*\0"), "image/tiff");
        assert_eq!(image_media_type(b"MM\0*"), "image/tiff");
        assert_eq!(image_media_type(b"MM\0"), "");
        assert_eq!(image_media_type(b"\xFF"), "");
        assert_eq!(image_media_type(b""), "");
    }

    #[test]
    fn item_types_and_normalized_values() {
        let mut items = item("TITLE", TEXT, b"Song");
        items.extend(item("Track", TEXT, b"3/12"));
        items.extend(item("Year", TEXT, b"2003-05-01"));
        items.extend(item("year", TEXT, b"May 2003"));
        items.extend(item("Record Date", TEXT, b"2002"));
        items.extend(item("EAN/UPC", TEXT, b"0123456789012"));
        items.extend(item("Album", TEXT, b""));
        items.extend(item("Custom", TEXT, "\u{416}".as_bytes()));
        items.extend(item("Data", BINARY, &[0, 1, 2]));
        items.extend(item("File", LOCATOR, b"a.flac\0b.flac"));
        items.extend(item("Odd", RESERVED | 1, b"x"));
        let revision = v2(11, &items).unwrap();
        let tags: Vec<_> = revision
            .tags
            .iter()
            .map(|t| (t.raw.key.as_str(), &t.raw.value, t.standard.clone()))
            .collect();
        let text = |s: &str| RawValue::Text(s.into());
        assert_eq!(
            tags,
            [
                (
                    "TITLE",
                    &text("Song"),
                    Some(StandardTag::TrackTitle("Song".into()))
                ),
                ("Track", &text("3/12"), Some(StandardTag::TrackNumber(3))),
                (
                    "Year",
                    &text("2003-05-01"),
                    Some(StandardTag::ReleaseDate("2003-05-01".into()))
                ),
                ("year", &text("May 2003"), None),
                (
                    "Record Date",
                    &text("2002"),
                    Some(StandardTag::RecordingDate("2002".into()))
                ),
                (
                    "EAN/UPC",
                    &text("0123456789012"),
                    Some(StandardTag::Barcode("0123456789012".into()))
                ),
                ("Album", &text(""), None),
                ("Custom", &text("\u{416}"), None),
                ("Data", &RawValue::Binary(vec![0, 1, 2]), None),
                (
                    "File",
                    &RawValue::MultiText(vec!["a.flac".into(), "b.flac".into()]),
                    None
                ),
                ("Odd", &RawValue::Binary(b"x".to_vec()), None),
            ]
        );
    }

    #[test]
    fn malformed_items() {
        let err = |count: u32, items: &[u8]| {
            let err = v2(count, items).unwrap_err();
            (err.kind, err.byte_offset.unwrap())
        };
        use ErrorKind::{InvalidData, UnexpectedEof};

        // The value runs past the items; the error points at the value.
        let mut bytes = item("Title", TEXT, b"abc");
        bytes.truncate(bytes.len() - 1);
        assert_eq!(err(1, &bytes), (UnexpectedEof, 14));
        let mut huge = item("Title", TEXT, b"");
        huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(err(1, &huge), (UnexpectedEof, 14));
        // More items announced than stored: accepted if the data ends at an
        // item boundary, an error if it ends inside an item.
        let revision = v2(5, &item("Title", TEXT, b"a")).unwrap();
        assert_eq!(revision.tags.len(), 1);
        assert_eq!(v2(3, &[]), Ok(MetadataRevision::default()));
        let mut cut = item("Title", TEXT, b"a");
        cut.extend_from_slice(&[0; 7]);
        assert_eq!(err(2, &cut), (UnexpectedEof, 15));
        assert_eq!(err(1, &[0; 7]), (UnexpectedEof, 0));
        // Keys.
        assert_eq!(err(1, &item("A", TEXT, b"")), (InvalidData, 8));
        assert_eq!(err(1, &item("TAG", TEXT, b"")), (InvalidData, 8));
        assert_eq!(err(1, &item("MP+", TEXT, b"")), (InvalidData, 8));
        assert_eq!(err(1, &item("Ti\x7Fle", TEXT, b"")), (InvalidData, 8));
        assert_eq!(err(1, &item("Ti\u{E9}", TEXT, b"")), (InvalidData, 8));
        assert!(v2(1, &item(&"K".repeat(255), TEXT, b"")).is_ok());
        assert_eq!(err(1, &item(&"K".repeat(256), TEXT, b"")), (InvalidData, 8));
        let mut unterminated = 0u32.to_le_bytes().repeat(2);
        unterminated.extend_from_slice(b"Title");
        assert_eq!(err(1, &unterminated), (UnexpectedEof, 8));
        // Values.
        assert_eq!(err(1, &item("Title", TEXT, b"\xFF")), (InvalidData, 14));
        assert_eq!(
            err(1, &item("Cover Art (Front)", BINARY, b"\xFF\xD8")),
            (InvalidData, 26)
        );
        assert_eq!(
            err(1, &item("Cover Art (Front)", BINARY, b"\xC3\0\xFF\xD8")),
            (InvalidData, 26)
        );
        // Bytes after the last item.
        let mut trailing = item("Title", TEXT, b"a");
        trailing.push(0);
        assert_eq!(err(1, &trailing), (InvalidData, 15));
        // Unknown version.
        let unknown = ApeHeader::parse(&header(3000, 32, 0, 0)).unwrap_err();
        assert_eq!(
            (unknown.kind, unknown.byte_offset),
            (ErrorKind::UnsupportedFeature, Some(8))
        );
        let mut bad_magic = header(VERSION_2, 32, 0, 0);
        bad_magic[7] = b'Y';
        assert_eq!(
            ApeHeader::parse(&bad_magic).unwrap_err().kind,
            ErrorKind::InvalidHeader
        );
    }

    #[test]
    fn field_limits() {
        // 65 536 empty values in one item are accepted, one more is not.
        let most = vec![0u8; 65_535];
        let revision = v2(1, &item("Artist", TEXT, &most)).unwrap();
        assert!(matches!(&revision.tags[0].raw.value, RawValue::MultiText(v) if v.len() == 65_536));
        let too_many = vec![0u8; 65_536];
        let err = v2(1, &item("Artist", TEXT, &too_many)).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::LimitExceeded, Some(15))
        );
        let err = v2(1, &item("File", LOCATOR, &too_many)).unwrap_err();
        assert_eq!(err.kind, ErrorKind::LimitExceeded);

        // 65 536 items are accepted, one more is not.
        let one = item("K1", BINARY, b"");
        let items = one.repeat(65_537);
        assert_eq!(
            v2(65_536, &items[..one.len() * 65_536]).unwrap().tags.len(),
            65_536
        );
        let err = v2(65_537, &items).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (
                ErrorKind::LimitExceeded,
                Some((one.len() * 65_536 + 11) as u64)
            )
        );
    }

    #[test]
    fn read_trailing_tag_with_header() {
        let items = item("Title", TEXT, b"Song");
        let size = items.len() as u32 + 32;
        let mut file = b"audio".to_vec();
        file.extend_from_slice(&header(
            VERSION_2,
            size,
            1,
            FLAG_HAS_HEADER | FLAG_IS_HEADER,
        ));
        file.extend_from_slice(&items);
        file.extend_from_slice(&header(VERSION_2, size, 1, FLAG_HAS_HEADER));
        let end = file.len() as u64;
        file.extend_from_slice(b"TAG after the APE tag");

        let mut reader = ByteReader::new(Cursor::new(&file));
        let mut budget = MetadataBudget::new(&FormatLimits::new());
        let tag = read_trailing(&mut reader, end, 5, &mut budget)
            .unwrap()
            .unwrap();
        assert_eq!(tag.start, 5);
        // Not before the floor.
        let err = read_trailing(&mut reader, end, 6, &mut budget).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidHeader);
        assert_eq!(tag.revision.tags[0].raw.key, "Title");

        // Item errors carry absolute offsets: the value starts at 5 + 32 + 14.
        let mut broken = file[..end as usize].to_vec();
        broken[5 + 32 + 14] = 0xFF;
        let err = read(&broken, &FormatLimits::new()).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidData, Some(51))
        );

        // The header announced by the footer must be there.
        let mut no_header = file[..end as usize].to_vec();
        no_header[5] = b'X';
        let err = read(&no_header, &FormatLimits::new()).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(5))
        );

        // The whole tag, header and footer included, is taken from the
        // metadata budget.
        let limits = FormatLimits::new().with_max_metadata_bytes(items.len() + 63);
        let err = read(&file[..end as usize], &limits).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::LimitExceeded, Some(5))
        );
        let limits = FormatLimits::new().with_max_metadata_bytes(items.len() + 64);
        assert!(read(&file[..end as usize], &limits).unwrap().is_some());
    }

    #[test]
    fn read_trailing_rejects_impossible_footers() {
        assert_eq!(read(b"", &FormatLimits::new()), Ok(None));
        assert_eq!(read(&[0; 31], &FormatLimits::new()), Ok(None));
        assert_eq!(read(&[0; 64], &FormatLimits::new()), Ok(None));

        // The tag would start before byte 0.
        let mut file = vec![0u8; 10];
        file.extend_from_slice(&header(VERSION_2, 100, 0, 0));
        let err = read(&file, &FormatLimits::new()).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(10))
        );
        let mut file = vec![0u8; 10];
        file.extend_from_slice(&header(VERSION_2, u32::MAX, 0, FLAG_HAS_HEADER));
        assert_eq!(
            read(&file, &FormatLimits::new()).unwrap_err().kind,
            ErrorKind::InvalidHeader
        );

        // A header where a footer must be.
        let file = header(VERSION_2, 32, 0, FLAG_IS_HEADER);
        let err = read(&file, &FormatLimits::new()).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidHeader, Some(0))
        );

        // Header errors carry absolute offsets.
        let mut file = vec![0u8; 3];
        file.extend_from_slice(&header(1, 32, 0, 0));
        let err = read(&file, &FormatLimits::new()).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::UnsupportedFeature, Some(11))
        );
    }
}
