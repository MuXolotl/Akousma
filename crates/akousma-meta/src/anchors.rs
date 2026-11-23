//! The tags around the audio data of a file.
//!
//! [`scan`] finds the tags in front of and after the audio data and returns
//! the byte range of the audio data. [`TagScanner`] plugs it into
//! [`Probe`](akousma_core::probe::Probe), which then gives each demuxer only
//! that range. `Probe` calls the two halves, [`scan_leading`] and
//! [`scan_trailing`], separately: trailing tags are searched only after the
//! end of the container when its header declares one, so that bytes inside
//! the audio data that look like a tag are not taken for one.
//!
//! **Leading tags.** Up to [`MAX_LEADING_TAGS`] ID3v2 and APEv2 tags in a
//! row are taken from the start of the file. Bytes `"ID3"` that do not form
//! a valid ID3v2 header (unknown version, a size byte of `0x80` or more), or
//! a header whose tag does not fit in the file, are left as audio data. The
//! footer of an APEv2 tag is found by its magic, not by flag bit 30.
//!
//! **Trailing tags.** Up to [`MAX_TRAILING_TAGS`] blocks are taken from the
//! end of the file, from the outside in, none of which may start before the
//! floor given to [`scan_trailing`]. At each position, an APE footer is
//! tried first, so that the `"TAG"` inside the header of an APE tag that
//! ends the file is not taken for an ID3v1 tag. Then an ID3v1 tag (once per
//! file) is tried, with the `TAG+` extension and the Lyrics3 v2.00 or v1.00
//! tag that may precede it. A trailing tag that fails to parse is ignored
//! and does not move the end of the audio data, so that a damaged tag never
//! makes the audio unplayable; an exhausted budget is not such a failure
//! (see below). Trailing revisions are returned in file order.
//!
//! **Errors of the source.** A tag is damaged only when all of its bytes
//! were read and do not form a valid tag, such as an item that runs past
//! the end of the tag. A source that fails, or that ends inside the length
//! `len` it reported, is not a damaged tag: the scan fails with its error
//! ([`ErrorKind::Io`], or [`ErrorKind::UnexpectedEof`] at the position of
//! the read), whatever [`DemuxOptions::read_metadata`] says.
//!
//! The chapters of leading ID3v2 tags go to [`Anchors::chapters`].
//!
//! **Budget.** Every scan takes from the [`MetadataBudget`] of the source,
//! which the container's metadata shares afterwards. With
//! [`DemuxOptions::read_metadata`], the whole stored size of every tag read
//! is taken from its bytes before the tag is read, and one entry per field,
//! picture and chapter kept.
//!
//! Whether an APEv2 or a Lyrics3 v2.00 tag is one depends on its items (an
//! orphan footer that points into the audio data is not), so without
//! `read_metadata` these tags are still checked, by the same rules: the
//! audio range does not depend on that option. They are then checked
//! through a fixed buffer, without being read into memory: the bytes read
//! are taken from the scanned bytes of the budget
//! ([`FormatLimits::max_tag_scan_bytes`](akousma_core::FormatLimits::max_tag_scan_bytes)),
//! and the bytes that no rule needs (pictures and other binary values, the
//! text of Lyrics3 fields) are skipped by seeking. An ID3v2 tag is bounded
//! by its header and an ID3v1 tag (with a `TAG+` extension) has a fixed
//! size: without `read_metadata` they are neither read nor counted. The
//! reads of fixed size (headers and footers, ID3v1 and `TAG+` tags, the
//! window searched for a Lyrics3 v1.00 tag) are never counted.
//!
//! A tag that does not fit in the budget is [`ErrorKind::LimitExceeded`]:
//! unlike a malformed tag it is not ignored, so a file never plays with
//! fewer tags than it has, or with a tag taken for audio, because of a
//! limit. The bytes checked without `read_metadata` never exceed the stored
//! size of the tags read with it, so with the same limits and
//! `max_tag_scan_bytes` not below `max_metadata_bytes` a scan that succeeds
//! with `read_metadata` succeeds without it and finds the same audio range.
//! Without `read_metadata` no revision or chapter is returned.

use std::io::{Read, Seek, SeekFrom};

use akousma_core::io::{ByteReader, MediaSource};
use akousma_core::meta::{MetadataBudget, MetadataRevision};
use akousma_core::probe::{AnchorScanner, Anchors};
use akousma_core::{DemuxOptions, Error, ErrorKind};

use crate::{Items, ape, id3v1, id3v2, lyrics3};

/// Largest number of tags taken from the start of a file.
pub const MAX_LEADING_TAGS: usize = 8;

/// Largest number of tags taken from the end of a file.
pub const MAX_TRAILING_TAGS: usize = 8;

/// An [`AnchorScanner`] for ID3v2, APE, ID3v1, `TAG+` and Lyrics3 tags.
///
/// ```
/// use akousma_core::probe::Probe;
/// use akousma_meta::anchors::TagScanner;
///
/// let mut probe = Probe::new();
/// probe.set_anchor_scanner(Box::new(TagScanner));
/// ```
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TagScanner;

impl AnchorScanner for TagScanner {
    fn scan_leading(
        &self,
        source: &mut dyn MediaSource,
        len: u64,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<Anchors, Error> {
        scan_leading(&mut ByteReader::new(source), len, options, budget)
    }

    fn scan_trailing(
        &self,
        source: &mut dyn MediaSource,
        anchors: &mut Anchors,
        floor: u64,
        options: &DemuxOptions,
        budget: &mut MetadataBudget,
    ) -> Result<(), Error> {
        scan_trailing(
            &mut ByteReader::new(source),
            anchors,
            floor,
            options,
            budget,
        )
    }
}

/// Finds the tags around the audio data of a source of `len` bytes:
/// [`scan_leading`], then [`scan_trailing`] with no floor beyond the start of
/// the audio data.
///
/// ```
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::{DemuxOptions, io::ByteReader};
/// use akousma_meta::anchors;
/// use std::io::Cursor;
///
/// let mut file = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec(); // empty ID3v2 tag
/// file.extend_from_slice(b"audio");
/// let mut id3v1 = [0u8; 128];
/// id3v1[..3].copy_from_slice(b"TAG");
/// file.extend_from_slice(&id3v1);
///
/// let len = file.len() as u64;
/// let options = DemuxOptions::new();
/// let mut budget = MetadataBudget::new(&options.limits);
/// let mut reader = ByteReader::new(Cursor::new(file));
/// let anchors = anchors::scan(&mut reader, len, &options, &mut budget)?;
/// assert_eq!((anchors.audio_start, anchors.audio_end), (10, 15));
/// assert_eq!((anchors.leading.len(), anchors.trailing.len()), (1, 1));
/// // Both tags were taken from the budget: 10 and 128 bytes.
/// assert_eq!(budget.remaining_bytes(), options.limits.max_metadata_bytes - 138);
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// The errors of the reader, including [`ErrorKind::UnexpectedEof`] if the
/// source ends before `len`, and [`ErrorKind::LimitExceeded`] if `budget`
/// runs out. A damaged tag is not an error.
pub fn scan<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    len: u64,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<Anchors, Error> {
    let mut anchors = scan_leading(reader, len, options, budget)?;
    let floor = anchors.audio_start;
    scan_trailing(reader, &mut anchors, floor, options, budget)?;
    Ok(anchors)
}

/// Finds the tags at the start of a source of `len` bytes. The returned
/// anchors end at `len` and have no trailing revisions.
///
/// # Errors
///
/// The errors of the reader, including [`ErrorKind::UnexpectedEof`] if the
/// source ends before `len`, and [`ErrorKind::LimitExceeded`] if `budget`
/// runs out. A damaged tag is not an error.
pub fn scan_leading<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    len: u64,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<Anchors, Error> {
    let mut anchors = Anchors {
        audio_start: 0,
        audio_end: len,
        ..Anchors::default()
    };
    take_leading(reader, &mut anchors, options, budget)?;
    Ok(anchors)
}

/// Finds the tags that end the audio range of `anchors` and start at or
/// after `floor` (and after [`Anchors::audio_start`]), moving
/// [`Anchors::audio_end`] to the start of the first one.
///
/// ```
/// use akousma_core::meta::MetadataBudget;
/// use akousma_core::{DemuxOptions, io::ByteReader};
/// use akousma_meta::anchors;
/// use std::io::Cursor;
///
/// // 128 bytes of audio data that happen to start with "TAG".
/// let mut file = vec![0u8; 128];
/// file[..3].copy_from_slice(b"TAG");
/// let len = file.len() as u64;
/// let options = DemuxOptions::new();
/// let mut budget = MetadataBudget::new(&options.limits);
/// let mut reader = ByteReader::new(Cursor::new(file));
/// let mut found = anchors::scan_leading(&mut reader, len, &options, &mut budget)?;
/// // A container that declares all 128 bytes: no tag can follow it.
/// anchors::scan_trailing(&mut reader, &mut found, 128, &options, &mut budget)?;
/// assert_eq!((found.audio_end, found.trailing.len()), (128, 0));
/// // Without a floor the bytes are taken for an ID3v1 tag.
/// anchors::scan_trailing(&mut reader, &mut found, 0, &options, &mut budget)?;
/// assert_eq!((found.audio_end, found.trailing.len()), (0, 1));
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// The errors of the reader, including [`ErrorKind::UnexpectedEof`] if the
/// source ends before [`Anchors::audio_end`], and
/// [`ErrorKind::LimitExceeded`] if `budget` runs out. A damaged tag is not an
/// error.
pub fn scan_trailing<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    anchors: &mut Anchors,
    floor: u64,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<(), Error> {
    take_trailing(
        reader,
        anchors,
        floor.max(anchors.audio_start),
        options,
        budget,
    )
}

/// Returns the tag parsed from bytes that were all read, or `None` if they
/// are not a valid tag. Exhausted limits are returned.
fn tolerate<T>(result: Result<T, Error>) -> Result<Option<T>, Error> {
    match result {
        Err(err) if err.kind == ErrorKind::LimitExceeded => Err(err),
        Err(_) => Ok(None),
        Ok(value) => Ok(Some(value)),
    }
}

/// Moves the offset of `err` by `base` bytes.
fn shift(err: Error, base: u64) -> Error {
    match err.byte_offset {
        Some(offset) => Error::at(err.kind, offset.saturating_add(base)),
        None => Error::at(err.kind, base),
    }
}

/// Whether the APEv2 and Lyrics3 v2.00 tags are parsed or only checked.
const fn items(options: &DemuxOptions) -> Items {
    if options.read_metadata {
        Items::Parse
    } else {
        Items::Check
    }
}

/// Takes the entries of `revision` with [`DemuxOptions::read_metadata`] and
/// adds it to `found`; the tag starts at `at`.
fn keep(
    revision: MetadataRevision,
    at: u64,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
    found: &mut Vec<MetadataRevision>,
) -> Result<(), Error> {
    if options.read_metadata {
        budget
            .take_revision(&revision)
            .map_err(|kind| Error::at(kind, at))?;
        found.push(revision);
    }
    Ok(())
}

/// Takes the tags at the start of the audio range.
fn take_leading<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    anchors: &mut Anchors,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<(), Error> {
    for _ in 0..MAX_LEADING_TAGS {
        let pos = anchors.audio_start;
        if anchors.audio_end - pos < id3v2::HEADER_LEN as u64 {
            break;
        }
        reader.seek(SeekFrom::Start(pos))?;
        let magic = reader.read_array::<{ id3v2::HEADER_LEN }>()?;
        if magic.starts_with(&id3v2::MAGIC) {
            let Ok(header) = id3v2::Id3v2Header::parse(&magic) else {
                break;
            };
            let end = pos + header.total_len();
            if end > anchors.audio_end {
                break;
            }
            if options.read_metadata {
                budget
                    .take_bytes(header.total_len())
                    .map_err(|kind| Error::at(kind, pos))?;
                reader.seek(SeekFrom::Start(pos))?;
                let tag = reader.read_bounded_vec(header.total_len(), usize::MAX)?;
                let parsed = id3v2::parse_with_budget(&tag, budget).map_err(|err| shift(err, pos));
                if let Some(tag) = tolerate(parsed)? {
                    anchors.leading.push(tag.revision);
                    anchors.chapters.extend(tag.chapters);
                }
            }
            anchors.audio_start = end;
        } else if magic.starts_with(&ape::MAGIC) {
            if anchors.audio_end - pos < ape::HEADER_LEN as u64 {
                break;
            }
            let found = ape::find_leading(reader, pos, anchors.audio_end, budget, items(options))?;
            let Some(tag) = found.tag() else {
                break;
            };
            keep(tag.revision, pos, options, budget, &mut anchors.leading)?;
            anchors.audio_start = tag.end;
        } else {
            break;
        }
    }
    Ok(())
}

/// Takes the tags at the end of the audio range that start at or after
/// `floor`.
fn take_trailing<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    anchors: &mut Anchors,
    floor: u64,
    options: &DemuxOptions,
    budget: &mut MetadataBudget,
) -> Result<(), Error> {
    let mut found: Vec<MetadataRevision> = Vec::new();
    let mut id3v1_seen = false;
    let mut taken = 0;
    while taken < MAX_TRAILING_TAGS {
        let end = anchors.audio_end;
        if let Some(tag) = ape::find_trailing(reader, end, floor, budget, items(options))?.tag() {
            keep(tag.revision, tag.start, options, budget, &mut found)?;
            anchors.audio_end = tag.start;
            taken += 1;
            continue;
        }
        if id3v1_seen {
            break;
        }
        let Some(tag) = read_id3v1(reader, end, floor)? else {
            break;
        };
        id3v1_seen = true;
        let mut start = end - id3v1::TAG_LEN as u64;
        let mut revision = id3v1::parse(&tag).ok();
        if let Some(extended_start) = start
            .checked_sub(id3v1::EXTENDED_TAG_LEN as u64)
            .filter(|&s| s >= floor)
        {
            reader.seek(SeekFrom::Start(extended_start))?;
            let extension = reader.read_array::<{ id3v1::EXTENDED_TAG_LEN }>()?;
            if let Ok(extended) = id3v1::parse_extended(&extension, &tag) {
                revision = Some(extended);
                start = extended_start;
            }
        }
        if options.read_metadata {
            budget
                .take_bytes(end - start)
                .map_err(|kind| Error::at(kind, start))?;
        }
        if let Some(revision) = revision {
            keep(revision, start, options, budget, &mut found)?;
        }
        anchors.audio_end = start;
        taken += 1;
        if taken == MAX_TRAILING_TAGS {
            break;
        }
        let lyrics = lyrics3::find_trailing_v2(reader, start, floor, &tag, budget, items(options))?;
        if let Some(lyrics) = lyrics.tag() {
            keep(lyrics.revision, lyrics.start, options, budget, &mut found)?;
            anchors.audio_end = lyrics.start;
            taken += 1;
        } else if let Some(lyrics_start) = lyrics3::find_v1_start(reader, start)?
            && lyrics_start >= floor
        {
            anchors.audio_end = lyrics_start;
            taken += 1;
        }
    }
    found.reverse();
    anchors.trailing = found;
    Ok(())
}

/// Reads the ID3v1 tag that ends at `end`, if there is one after `floor`.
fn read_id3v1<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
) -> Result<Option<[u8; id3v1::TAG_LEN]>, Error> {
    let Some(start) = end
        .checked_sub(id3v1::TAG_LEN as u64)
        .filter(|&s| s >= floor)
    else {
        return Ok(None);
    };
    reader.seek(SeekFrom::Start(start))?;
    let tag = reader.read_array::<{ id3v1::TAG_LEN }>()?;
    Ok(tag.starts_with(&id3v1::MAGIC).then_some(tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use akousma_core::FormatLimits;
    use akousma_core::meta::RawValue;
    use std::io::Cursor;

    mod source_errors;
    mod tag_scan;

    /// An APE header or footer.
    fn ape_structure(tag_size: u32, items: u32, flags: u32) -> Vec<u8> {
        let mut bytes = ape::MAGIC.to_vec();
        for field in [ape::VERSION_2, tag_size, items, flags, 0, 0] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        bytes
    }

    /// One APE text item.
    fn ape_item(key: &str, value: &str) -> Vec<u8> {
        let mut item = u32::try_from(value.len()).unwrap().to_le_bytes().to_vec();
        item.extend_from_slice(&0u32.to_le_bytes());
        item.extend_from_slice(key.as_bytes());
        item.push(0);
        item.extend_from_slice(value.as_bytes());
        item
    }

    /// An APEv2 tag: optional header, items, optional footer.
    fn ape_tag(item: &[u8], header_flags: Option<u32>, footer: bool) -> Vec<u8> {
        let footer_len = if footer { 32 } else { 0 };
        let tag_size = u32::try_from(item.len() + footer_len).unwrap();
        let has_header = if header_flags.is_some() { 1 << 31 } else { 0 };
        let mut tag = Vec::new();
        if let Some(flags) = header_flags {
            tag.extend(ape_structure(tag_size, 1, flags | 1 << 29 | 1 << 31));
        }
        tag.extend_from_slice(item);
        if footer {
            tag.extend(ape_structure(tag_size, 1, has_header));
        }
        tag
    }

    fn id3v1_tag(title: &str) -> Vec<u8> {
        let mut tag = vec![0u8; id3v1::TAG_LEN];
        tag[..3].copy_from_slice(b"TAG");
        tag[3..3 + title.len()].copy_from_slice(title.as_bytes());
        tag[127] = 255;
        tag
    }

    fn lyrics3v2_tag() -> Vec<u8> {
        let mut tag = b"LYRICSBEGINLYR00005Hello".to_vec();
        let size = format!("{:06}", tag.len());
        tag.extend_from_slice(size.as_bytes());
        tag.extend_from_slice(b"LYRICS200");
        tag
    }

    fn first_text(revision: &MetadataRevision) -> String {
        match &revision.tags[0].raw.value {
            RawValue::Text(text) => text.clone(),
            other => format!("{other:?}"),
        }
    }

    fn run(file: &[u8], options: &DemuxOptions) -> Anchors {
        try_run(file, options).unwrap()
    }

    fn try_run(file: &[u8], options: &DemuxOptions) -> Result<Anchors, Error> {
        let mut reader = ByteReader::new(Cursor::new(file));
        let mut budget = MetadataBudget::new(&options.limits);
        scan(&mut reader, file.len() as u64, options, &mut budget)
    }

    fn full_budget() -> MetadataBudget {
        MetadataBudget::new(&FormatLimits::new())
    }

    #[test]
    fn ape_header_first_footer_by_magic() {
        let item = ape_item("Title", "Lead");
        // Header flags with bit 30 set (no footer, as some writers mark tags
        // that do have one) and clear, with and without a footer.
        for (bit_30, footer) in [(1 << 30, true), (0, true), (1 << 30, false), (0, false)] {
            let mut file = ape_tag(&item, Some(bit_30), footer);
            let tag_len = file.len() as u64;
            file.extend_from_slice(b"audio data");
            let mut reader = ByteReader::new(Cursor::new(&file));
            let tag = ape::read_leading(&mut reader, 0, u64::MAX, &mut full_budget())
                .unwrap()
                .unwrap();
            assert_eq!(tag.end, tag_len, "bit 30 {bit_30:#x}, footer {footer}");
            assert_eq!(first_text(&tag.revision), "Lead");

            let anchors = run(&file, &DemuxOptions::new());
            assert_eq!(
                (anchors.audio_start, anchors.audio_end),
                (tag_len, file.len() as u64)
            );
            assert_eq!(anchors.leading.len(), 1);
        }
        // An APEv1 structure or a footer cannot start a tag.
        let mut file = ape_tag(&item, None, true);
        file.rotate_right(32);
        let mut reader = ByteReader::new(Cursor::new(&file));
        let err = ape::read_leading(&mut reader, 0, u64::MAX, &mut full_budget()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidHeader);
        let mut reader =
            ByteReader::new(Cursor::new(b"not a tag at all, and no magic here either"));
        assert_eq!(
            ape::read_leading(&mut reader, 0, u64::MAX, &mut full_budget()),
            Ok(None)
        );
    }

    #[test]
    fn anchors_ape_before_id3v1_and_false_tag() {
        // A 131-byte APE tag with a header ends the file: the `TAG` of its
        // header lies exactly 128 bytes before the end.
        let item = ape_item("Title", &"x".repeat(131 - 64 - 14));
        let tag = ape_tag(&item, Some(0), true);
        assert_eq!(tag.len(), 131);
        let mut file = b"audio".to_vec();
        file.extend_from_slice(&tag);
        assert_eq!(&file[file.len() - 128..file.len() - 125], b"TAG");
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!(anchors.audio_end, 5);
        assert_eq!(anchors.trailing.len(), 1);
        assert_eq!(anchors.trailing[0].tags[0].raw.key, "Title");

        // audio | APEv2 | Lyrics3v2 | ID3v1, trailing revisions in file order.
        let mut file = b"audio".to_vec();
        file.extend(ape_tag(&ape_item("Title", "Ape"), Some(0), true));
        file.extend(lyrics3v2_tag());
        file.extend(id3v1_tag("V1"));
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!(anchors.audio_end, 5);
        let texts: Vec<_> = anchors.trailing.iter().map(first_text).collect();
        assert_eq!(texts, ["Ape", "Hello", "V1"]);
        // Without metadata the range is the same and nothing is returned.
        let quiet = run(&file, &DemuxOptions::new().with_read_metadata(false));
        assert_eq!((quiet.audio_end, quiet.trailing.len()), (5, 0));

        // audio | ID3v1 | APEv2: an APE tag written after the ID3v1 tag.
        let mut file = b"audio".to_vec();
        file.extend(id3v1_tag("V1"));
        file.extend(ape_tag(&ape_item("Title", "Ape"), None, true));
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!(anchors.audio_end, 5);
        let texts: Vec<_> = anchors.trailing.iter().map(first_text).collect();
        assert_eq!(texts, ["V1", "Ape"]);

        // An orphan APE footer whose size points into the audio data, and
        // one larger than the file: ignored, the audio range is unchanged.
        for tag_size in [40, 4000] {
            let mut file = vec![0x55; 64];
            file.extend(ape_structure(tag_size, 3, 0));
            let anchors = run(&file, &DemuxOptions::new());
            assert_eq!((anchors.audio_start, anchors.audio_end), (0, 96));
            assert!(anchors.trailing.is_empty());
        }

        // Lyrics3 v1.00 before ID3v1 only moves the boundary.
        let mut file = b"audioLYRICSBEGINla laLYRICSEND".to_vec();
        file.extend(id3v1_tag("V1"));
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!((anchors.audio_end, anchors.trailing.len()), (5, 1));

        // TAG+ before ID3v1.
        let mut extension = vec![0u8; id3v1::EXTENDED_TAG_LEN];
        extension[..4].copy_from_slice(b"TAG+");
        let mut file = b"audio".to_vec();
        file.extend(extension);
        file.extend(id3v1_tag("V1"));
        assert_eq!(run(&file, &DemuxOptions::new()).audio_end, 5);
    }

    #[test]
    fn leading_tags_stack_and_false_id3() {
        let id3 = |major: u8, size: u8| {
            let mut tag = vec![b'I', b'D', b'3', major, 0, 0, 0, 0, 0, size];
            tag.resize(10 + usize::from(size), 0);
            tag
        };
        // Two ID3v2 tags and an APE tag in a row.
        let mut file = id3(3, 4);
        file.extend(id3(4, 0));
        file.extend(ape_tag(&ape_item("Title", "Lead"), Some(0), true));
        let start = file.len() as u64;
        file.extend_from_slice(b"fLaC and more audio");
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!(anchors.audio_start, start);
        assert_eq!(anchors.leading.len(), 3);

        // False "ID3": bad version, non-synchsafe size, tag past the end.
        for bad in [
            b"ID3\xFF\x00\x00\x00\x00\x00\x00audio".to_vec(),
            b"ID3\x03\x00\x00\x00\x00\x80\x00audio".to_vec(),
            b"ID3\x03\x00\x00\x00\x00\x01\x00audio".to_vec(),
        ] {
            let anchors = run(&bad, &DemuxOptions::new());
            assert_eq!(anchors.audio_start, 0);
            assert!(anchors.leading.is_empty());
        }

        // More than eight tags: the ninth is left as audio.
        let file: Vec<u8> = (0..10).flat_map(|_| id3(3, 0)).collect();
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!(anchors.audio_start, 80);
    }

    #[test]
    fn leading_id3v2_chapters() {
        let mut payload = b"ch1\0".to_vec();
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&1500u32.to_be_bytes());
        payload.extend_from_slice(&[0xFF; 8]);
        let mut frame = b"CHAP".to_vec();
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend(payload);
        let mut file = vec![b'I', b'D', b'3', 3, 0, 0, 0, 0, 0, frame.len() as u8];
        file.extend(frame);
        file.extend_from_slice(b"audio");
        let anchors = run(&file, &DemuxOptions::new());
        assert_eq!(anchors.chapters.len(), 1);
        assert_eq!(anchors.chapters[0].id, "ch1");
        let quiet = run(&file, &DemuxOptions::new().with_read_metadata(false));
        assert!(quiet.chapters.is_empty());
        assert_eq!(quiet.audio_start, anchors.audio_start);
    }

    #[test]
    fn one_budget_for_both_ends() {
        // A 22-byte ID3v2 tag and an APE tag in front, an APE tag and an
        // ID3v1 tag after the audio data.
        let id3 = b"ID3\x03\x00\x00\x00\x00\x00\x0cTIT2\x00\x00\x00\x02\x00\x00\x00L".to_vec();
        let lead = ape_tag(&ape_item("Title", "Lead"), Some(0), true);
        let tail = ape_tag(&ape_item("Title", "Tail"), None, true);
        let file = [
            id3.clone(),
            lead.clone(),
            b"audio".to_vec(),
            tail.clone(),
            id3v1_tag("V1"),
        ]
        .concat();
        let start = (id3.len() + lead.len()) as u64;
        let all = id3.len() + lead.len() + tail.len() + id3v1::TAG_LEN;
        let with = |bytes: usize, read_metadata: bool| {
            let limits = FormatLimits::new().with_max_metadata_bytes(bytes);
            let options = DemuxOptions::new()
                .with_limits(limits)
                .with_read_metadata(read_metadata);
            try_run(&file, &options).map(|a| {
                let range = (a.audio_start, a.audio_end);
                (range, a.leading.len(), a.trailing.len())
            })
        };
        let range = (start, start + 5);
        assert_eq!(with(all, true), Ok((range, 2, 2)));
        for short in [all - 1, all - id3v1::TAG_LEN, id3.len() - 1, 0] {
            let err = with(short, true).unwrap_err();
            assert_eq!(err.kind, ErrorKind::LimitExceeded, "{short}");
        }
        // Without metadata the byte limit is not used: the items of the APE
        // tags are checked within the separate scanned bytes, and the audio
        // range is the same.
        assert_eq!(with(0, false), Ok((range, 0, 0)));
        let items = 2 * ape_item("Title", "Lead").len();
        let scan = |bytes: usize| {
            let limits = FormatLimits::new()
                .with_max_metadata_bytes(0)
                .with_max_tag_scan_bytes(bytes);
            let options = DemuxOptions::new()
                .with_limits(limits)
                .with_read_metadata(false);
            try_run(&file, &options).map(|a| (a.audio_start, a.audio_end))
        };
        assert_eq!(scan(items), Ok(range));
        // One byte less: the trailing tag, checked last, does not fit.
        let err = scan(items - 1).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::LimitExceeded, Some(start + 5))
        );
        // With metadata the scanned bytes are not used.
        let limits = FormatLimits::new().with_max_tag_scan_bytes(0);
        let options = DemuxOptions::new().with_limits(limits);
        assert_eq!(try_run(&file, &options).map(|a| a.audio_end), Ok(start + 5));
    }

    #[test]
    fn trailing_entries_share_the_budget() {
        let file = [b"audio".to_vec(), id3v1_tag("V1")].concat();
        let mut reader = ByteReader::new(Cursor::new(&file));
        let options = DemuxOptions::new();
        let mut budget = full_budget();
        let len = file.len() as u64;
        // The ID3v1 tag has a title and a genre field.
        let fields = run(&file, &options).trailing[0].tags.len();
        budget
            .take_entries(MetadataBudget::MAX_ENTRIES - fields + 1)
            .unwrap();
        let err = scan(&mut reader, len, &options, &mut budget).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::LimitExceeded, Some(5))
        );
        let mut budget = full_budget();
        budget
            .take_entries(MetadataBudget::MAX_ENTRIES - fields)
            .unwrap();
        assert!(scan(&mut reader, len, &options, &mut budget).is_ok());
        assert_eq!(budget.remaining_entries(), 0);
    }
}
