//! Lyrics3 tags, which sit right before an ID3v1 tag.
//!
//! # Lyrics3 v2.00
//!
//! A Lyrics3 v2.00 tag is `"LYRICSBEGIN"`, a list of fields, the size of the
//! tag up to here as six ASCII digits, and `"LYRICS200"`. Each field is a
//! three-letter identifier, its size as five ASCII digits and its ISO-8859-1
//! text. The raw key is the identifier:
//!
//! | Field | Content | Normalized |
//! | :--- | :--- | :--- |
//! | `IND` | indications | none |
//! | `LYR` | lyrics | [`Lyrics`](StandardTag::Lyrics) |
//! | `INF` | additional information | [`Comment`](StandardTag::Comment) |
//! | `AUT` | author of the lyrics | [`Lyricist`](StandardTag::Lyricist) |
//! | `EAL`, `EAR`, `ETT` | extended album, artist and title | [`Album`](StandardTag::Album), [`Artist`](StandardTag::Artist), [`TrackTitle`](StandardTag::TrackTitle), if their first 30 bytes match the ID3v1 field |
//! | other | for example `IMG` (image links) | none |
//!
//! # Lyrics3 v1.00
//!
//! A Lyrics3 v1.00 tag is `"LYRICSBEGIN"`, at most 5100 bytes of lyrics and
//! `"LYRICSEND"`. Its fields are not parsed: [`find_v1_start`] only locates
//! the tag, so that it is not mistaken for audio data.
use std::io::{Read, Seek, SeekFrom};

use akousma_core::io::ByteReader;
use akousma_core::meta::{MetadataBudget, MetadataRevision, RawValue, StandardTag, Tag};
use akousma_core::{Error, ErrorKind};

use crate::encoding::{latin1, split_nul};
use crate::id3v1;
use crate::stream::{BUFFER_LEN, TagStream};
use crate::{Found, Items, MAX_REVISION_FIELDS, TextTag, TrailingTag, check_room, new_tag, parsed};

/// First bytes of a Lyrics3 tag.
pub const BEGIN: [u8; 11] = *b"LYRICSBEGIN";

/// Last bytes of a Lyrics3 v2.00 tag.
pub const V2_END: [u8; 9] = *b"LYRICS200";

/// Last bytes of a Lyrics3 v1.00 tag.
pub const V1_END: [u8; 9] = *b"LYRICSEND";

/// Largest number of lyrics bytes of a Lyrics3 v1.00 tag.
pub const V1_MAX_LYRICS: usize = 5100;

/// Number of ASCII digits of the Lyrics3 v2.00 tag size.
const V2_SIZE_DIGITS: usize = 6;

/// Size digits plus [`V2_END`].
const V2_FOOTER_LEN: usize = V2_SIZE_DIGITS + V2_END.len();

/// Field identifier and size digits that start every Lyrics3 v2.00 field.
const V2_FIELD_ID_LEN: usize = 3;
const V2_FIELD_SIZE_DIGITS: usize = 5;

/// Largest size of the `AUT`, `EAL`, `EAR` and `ETT` fields.
const V2_SHORT_FIELD_MAX: usize = 250;

/// Bytes of an ID3v1 field compared with the start of an extended field.
const ID3V1_FIELD_LEN: usize = 30;

/// Parses a Lyrics3 v2.00 tag and returns its fields.
///
/// `tag` holds the whole tag, from [`BEGIN`] to [`V2_END`].
/// `id3v1` is the ID3v1 tag that follows it, needed by the extended `EAL`,
/// `EAR` and `ETT` fields.
///
/// ```
/// use akousma_core::meta::StandardTag;
/// use akousma_meta::lyrics3;
///
/// let mut tag = b"LYRICSBEGININD00002".to_vec();
/// tag.extend_from_slice(b"10LYR00012Hello\r\nWorld");
/// let size = format!("{:06}", tag.len());
/// tag.extend_from_slice(size.as_bytes());
/// tag.extend_from_slice(b"LYRICS200");
///
/// let mut id3v1 = [0u8; 128];
/// id3v1[..3].copy_from_slice(b"TAG");
///
/// let revision = lyrics3::parse_v2(&tag, &id3v1)?;
/// assert_eq!(revision.tags[0].raw.key, "IND");
/// assert_eq!(revision.tags[1].standard, Some(StandardTag::Lyrics("Hello\r\nWorld".into())));
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// - [`ErrorKind::InvalidHeader`] if `tag` lacks [`BEGIN`] or
///   [`V2_END`], if the stored size is not six digits or differs from
///   the length of `tag`, or (at offset `tag.len()`) if `id3v1` does not
///   start with [`id3v1::MAGIC`].
/// - [`ErrorKind::UnexpectedEof`] if a field does not fit in the tag.
/// - [`ErrorKind::InvalidData`] for a field identifier that is not three
///   uppercase ASCII letters, a size that is not five digits, an `AUT`,
///   `EAL`, `EAR` or `ETT` field longer than 250 bytes, or a tag without
///   fields.
/// - [`ErrorKind::LimitExceeded`] for more than 65 536 fields.
pub fn parse_v2(tag: &[u8], id3v1: &[u8; id3v1::TAG_LEN]) -> Result<MetadataRevision, Error> {
    parse_v2_at(tag, id3v1, 0)
}

/// Reads a Lyrics3 v2.00 tag whose last byte is at `end - 1`, where `end` is
/// the position of the ID3v1 tag `id3v1`, and that starts at or after
/// `floor`.
///
/// Returns `Ok(None)` if the bytes before `end` do not end with
/// [`V2_END`]. Otherwise the stored size of the tag is taken from the byte
/// limit of `budget` before it is read; entries are not taken.
///
/// # Errors
///
/// - [`ErrorKind::InvalidHeader`] if the stored size is not six digits or
///   the tag would start before `floor`.
/// - [`ErrorKind::LimitExceeded`] at the start of the tag if it is larger
///   than what is left of `budget`, which is then unchanged.
/// - The errors of [`parse_v2`], with absolute offsets, and the
///   errors of the reader.
///
/// Both a field that runs past the end of the tag and a source that ends
/// inside the tag are [`ErrorKind::UnexpectedEof`] here; the scanner of
/// [`anchors`](crate::anchors) tells them apart and fails only on the
/// second.
pub fn read_trailing_v2<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
    id3v1: &[u8; id3v1::TAG_LEN],
    budget: &mut MetadataBudget,
) -> Result<Option<TrailingTag>, Error> {
    find_trailing_v2(reader, end, floor, id3v1, budget, Items::Parse)?.into_result()
}

/// [`read_trailing_v2`] that tells a damaged tag from an error of the
/// source. With [`Items::Check`] the revision is empty.
pub(crate) fn find_trailing_v2<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
    id3v1: &[u8; id3v1::TAG_LEN],
    budget: &mut MetadataBudget,
    items: Items,
) -> Result<Found<TrailingTag>, Error> {
    let start = match v2_start(reader, end, floor)? {
        Found::Tag(start) => start,
        Found::Absent => return Ok(Found::Absent),
        Found::Damaged(err) => return Ok(Found::Damaged(err)),
    };
    if items == Items::Check {
        let checked = check_v2(reader, start, end, id3v1, budget, &mut [0; BUFFER_LEN])?;
        return Ok(checked.map(|()| TrailingTag {
            start,
            revision: MetadataRevision::default(),
        }));
    }
    budget
        .take_bytes(end - start)
        .map_err(|kind| Error::at(kind, start))?;
    reader.seek(SeekFrom::Start(start))?;
    let tag = reader.read_bounded_vec(end - start, usize::MAX)?;
    Ok(parsed(parse_v2_at(&tag, id3v1, start))?.map(|revision| TrailingTag { start, revision }))
}

/// Finds the start of the Lyrics3 v2.00 tag whose footer ends at `end`, as
/// its stored size says, if the tag starts at or after `floor`.
pub(crate) fn v2_start<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
    floor: u64,
) -> Result<Found<u64>, Error> {
    let Some(footer_pos) = end.checked_sub(V2_FOOTER_LEN as u64) else {
        return Ok(Found::Absent);
    };
    reader.seek(SeekFrom::Start(footer_pos))?;
    let footer = reader.read_array::<V2_FOOTER_LEN>()?;
    let (digits, magic) = footer.split_at(V2_SIZE_DIGITS);
    if magic != V2_END {
        return Ok(Found::Absent);
    }
    let start = ascii_number(digits).and_then(|size| {
        footer_pos
            .checked_sub(size as u64)
            .filter(|&start| size >= BEGIN.len() && start >= floor)
    });
    Ok(match start {
        Some(start) => Found::Tag(start),
        None => Found::Damaged(Error::at(ErrorKind::InvalidHeader, footer_pos)),
    })
}

/// Checks the Lyrics3 v2.00 tag `start..end`, located by [`v2_start`], by
/// the rules of [`parse_v2`], reading it through `buf` (which must not be
/// empty) and keeping nothing. The result is that of parsing the tag, errors
/// and offsets included, except that the byte limit of `budget` is not used:
/// every read is taken from its scanned bytes instead, and errors of the
/// source are those of the bytes actually read. Only [`BEGIN`] and the field
/// headers are needed: the text of the fields is skipped by seeking, and the
/// footer was read by [`v2_start`].
pub(crate) fn check_v2<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    start: u64,
    end: u64,
    id3v1: &[u8; id3v1::TAG_LEN],
    budget: &mut MetadataBudget,
    buf: &mut [u8],
) -> Result<Found<()>, Error> {
    let damaged = |kind, at| Ok(Found::Damaged(Error::at(kind, at)));
    let fields_end = end.saturating_sub(V2_FOOTER_LEN as u64);
    let mut tag = TagStream::new(reader, budget, start, start, fields_end, buf);
    if tag.remaining() < BEGIN.len() as u64 {
        return damaged(ErrorKind::InvalidHeader, start);
    }
    let mut begin = [0; BEGIN.len()];
    tag.read_exact(&mut begin)?;
    if begin != BEGIN {
        return damaged(ErrorKind::InvalidHeader, start);
    }
    if !id3v1.starts_with(&id3v1::MAGIC) {
        return damaged(ErrorKind::InvalidHeader, end);
    }
    let mut fields = 0usize;
    while tag.remaining() > 0 {
        let field_start = tag.position();
        if fields >= MAX_REVISION_FIELDS {
            return Err(Error::at(ErrorKind::LimitExceeded, field_start));
        }
        if tag.remaining() < (V2_FIELD_ID_LEN + V2_FIELD_SIZE_DIGITS) as u64 {
            return damaged(ErrorKind::UnexpectedEof, field_start);
        }
        let mut header = [0; V2_FIELD_ID_LEN + V2_FIELD_SIZE_DIGITS];
        tag.read_exact(&mut header)?;
        let (id, digits) = header.split_at(V2_FIELD_ID_LEN);
        if !id.iter().all(u8::is_ascii_uppercase) {
            return damaged(ErrorKind::InvalidData, field_start);
        }
        let Some(len) = ascii_number(digits) else {
            return damaged(ErrorKind::InvalidData, field_start);
        };
        if len as u64 > tag.remaining() {
            return damaged(ErrorKind::UnexpectedEof, tag.position());
        }
        if is_short_field(id) && len > V2_SHORT_FIELD_MAX {
            return damaged(ErrorKind::InvalidData, field_start);
        }
        tag.skip(len as u64);
        fields += 1;
    }
    if fields == 0 {
        return damaged(ErrorKind::InvalidData, start + BEGIN.len() as u64);
    }
    Ok(Found::Tag(()))
}

/// Returns `true` for the fields of at most [`V2_SHORT_FIELD_MAX`] bytes.
fn is_short_field(id: &[u8]) -> bool {
    matches!(id, b"AUT" | b"EAL" | b"EAR" | b"ETT")
}

/// Locates a Lyrics3 v1.00 tag whose last byte is at `end - 1`, where `end`
/// is normally the position of an ID3v1 tag, and returns the position of its
/// first byte.
///
/// Returns `Ok(None)` if the bytes before `end` do not end with [`V1_END`],
/// or if no [`BEGIN`] is found in the [`V1_MAX_LYRICS`] bytes before it. The
/// search goes backwards from [`V1_END`], so the nearest [`BEGIN`] wins. The
/// lyrics themselves are not parsed.
///
/// ```
/// use akousma_core::io::ByteReader;
/// use akousma_meta::lyrics3;
/// use std::io::Cursor;
///
/// let file = b"audioLYRICSBEGINSome lyricsLYRICSENDTAG".to_vec();
/// let mut reader = ByteReader::new(Cursor::new(&file));
/// assert_eq!(lyrics3::find_v1_start(&mut reader, 36)?, Some(5));
/// assert_eq!(lyrics3::find_v1_start(&mut reader, 39)?, None);
/// # Ok::<(), akousma_core::Error>(())
/// ```
///
/// # Errors
///
/// The errors of the reader.
pub fn find_v1_start<R: Read + Seek>(
    reader: &mut ByteReader<R>,
    end: u64,
) -> Result<Option<u64>, Error> {
    let Some(footer_pos) = end.checked_sub(V1_END.len() as u64) else {
        return Ok(None);
    };
    reader.seek(SeekFrom::Start(footer_pos))?;
    if reader.read_array::<{ V1_END.len() }>()? != V1_END {
        return Ok(None);
    }
    let window = footer_pos.min((V1_MAX_LYRICS + BEGIN.len()) as u64);
    let window_start = footer_pos - window;
    reader.seek(SeekFrom::Start(window_start))?;
    // At most 5111 bytes, below any metadata limit.
    let bytes = reader.read_bounded_vec(window, usize::MAX)?;
    Ok(bytes
        .windows(BEGIN.len())
        .rposition(|candidate| candidate == BEGIN)
        .map(|offset| window_start + offset as u64))
}

/// Value of a field of ASCII digits, or `None` if a byte is not a digit.
fn ascii_number(digits: &[u8]) -> Option<usize> {
    digits.iter().try_fold(0usize, |value, &b| {
        b.is_ascii_digit()
            .then(|| value * 10 + usize::from(b - b'0'))
    })
}

/// Parses a Lyrics3 v2.00 tag; `base` is the absolute offset of `tag[0]`.
pub(crate) fn parse_v2_at(
    tag: &[u8],
    id3v1: &[u8; id3v1::TAG_LEN],
    base: u64,
) -> Result<MetadataRevision, Error> {
    let at = |pos: usize| base.saturating_add(pos as u64);
    let invalid_header = |pos: usize| Error::at(ErrorKind::InvalidHeader, at(pos));
    if !tag.starts_with(&BEGIN) {
        return Err(invalid_header(0));
    }
    let fields_end = tag
        .len()
        .checked_sub(V2_FOOTER_LEN)
        .filter(|&end| end >= BEGIN.len())
        .ok_or(invalid_header(0))?;
    let (digits, magic) = tag[fields_end..].split_at(V2_SIZE_DIGITS);
    if magic != V2_END {
        return Err(invalid_header(fields_end + V2_SIZE_DIGITS));
    }
    if ascii_number(digits) != Some(fields_end) {
        return Err(invalid_header(fields_end));
    }
    if !id3v1.starts_with(&id3v1::MAGIC) {
        return Err(invalid_header(tag.len()));
    }

    let mut revision = MetadataRevision::default();
    let mut pos = BEGIN.len();
    while pos < fields_end {
        let field_start = pos;
        let invalid = Error::at(ErrorKind::InvalidData, at(field_start));
        check_room(&revision).map_err(|kind| Error::at(kind, at(field_start)))?;
        let header_len = V2_FIELD_ID_LEN + V2_FIELD_SIZE_DIGITS;
        let header = tag[..fields_end]
            .get(pos..pos + header_len)
            .ok_or(Error::at(ErrorKind::UnexpectedEof, at(field_start)))?;
        let (id, digits) = header.split_at(V2_FIELD_ID_LEN);
        if !id.iter().all(u8::is_ascii_uppercase) {
            return Err(invalid);
        }
        let len = ascii_number(digits).ok_or(invalid)?;
        pos += header_len;
        let data = tag[..fields_end]
            .get(pos..pos + len)
            .ok_or(Error::at(ErrorKind::UnexpectedEof, at(pos)))?;
        pos += len;
        let id = core::str::from_utf8(id).map_err(|_| invalid)?;
        revision
            .tags
            .push(v2_field(id, data, id3v1).ok_or(invalid)?);
    }
    if revision.tags.is_empty() {
        return Err(Error::at(ErrorKind::InvalidData, at(BEGIN.len())));
    }
    Ok(revision)
}

/// Builds the tag of one Lyrics3 v2.00 field, or `None` if a short field is
/// too long.
fn v2_field(id: &str, data: &[u8], id3v1: &[u8; id3v1::TAG_LEN]) -> Option<Tag> {
    if is_short_field(id.as_bytes()) && data.len() > V2_SHORT_FIELD_MAX {
        return None;
    }
    // Extended fields apply only if they start with the ID3v1 field they
    // extend.
    let extends = |field: core::ops::Range<usize>| starts_like(data, &id3v1[field]);
    let normalize: Option<TextTag> = match id {
        "LYR" => Some(StandardTag::Lyrics),
        "INF" => Some(StandardTag::Comment),
        "AUT" => Some(StandardTag::Lyricist),
        "EAL" if extends(63..93) => Some(StandardTag::Album),
        "EAR" if extends(33..63) => Some(StandardTag::Artist),
        "ETT" if extends(3..33) => Some(StandardTag::TrackTitle),
        _ => None,
    };
    let text = latin1(data).into_owned();
    let standard = normalize
        .filter(|_| !text.is_empty())
        .map(|make| make(text.clone()));
    Some(new_tag(id, RawValue::Text(text), standard))
}

/// Returns `true` if the first 30 bytes of `extended` equal the ID3v1
/// `field`, both cut at their first `0x00` and without trailing spaces.
fn starts_like(extended: &[u8], field: &[u8]) -> bool {
    /// Bytes before the first `0x00`, without trailing spaces.
    fn trim(bytes: &[u8]) -> &[u8] {
        let bytes = split_nul(bytes).0;
        let len = bytes
            .iter()
            .rposition(|&b| b != b' ')
            .map_or(0, |last| last + 1);
        &bytes[..len]
    }
    trim(&extended[..extended.len().min(ID3V1_FIELD_LEN)]) == trim(field)
}

#[cfg(test)]
mod tests {
    use super::*;
    use akousma_core::FormatLimits;
    use std::io::Cursor;

    fn lyrics3v2(fields: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tag = BEGIN.to_vec();
        for (id, data) in fields {
            tag.extend_from_slice(id.as_bytes());
            tag.extend_from_slice(format!("{:05}", data.len()).as_bytes());
            tag.extend_from_slice(data);
        }
        tag.extend_from_slice(format!("{:06}", tag.len()).as_bytes());
        tag.extend_from_slice(&V2_END);
        tag
    }

    fn id3v1_tag(title: &[u8], artist: &[u8], album: &[u8]) -> [u8; id3v1::TAG_LEN] {
        let mut tag = [0u8; id3v1::TAG_LEN];
        tag[..3].copy_from_slice(&id3v1::MAGIC);
        tag[3..3 + title.len()].copy_from_slice(title);
        tag[33..33 + artist.len()].copy_from_slice(artist);
        tag[63..63 + album.len()].copy_from_slice(album);
        tag[127] = 255;
        tag
    }

    #[test]
    fn v2_fields() {
        let artist = b"An artist name longer than thirty bytes";
        let id3v1 = id3v1_tag(b"Short title   ", &artist[..30], b"Album");
        let tag = lyrics3v2(&[
            ("IND", b"11"),
            ("LYR", b"[00:01]One\r\n[00:02]Two"),
            ("INF", b"Caf\xE9"),
            ("AUT", b"Writer"),
            ("EAR", artist),
            ("ETT", b"Another title"),
            ("EAL", b"Album"),
            ("IMG", b""),
            ("XYZ", b"data"),
        ]);
        let revision = parse_v2(&tag, &id3v1).unwrap();
        let tags: Vec<_> = revision
            .tags
            .iter()
            .map(|t| (t.raw.key.as_str(), &t.raw.value, t.standard.clone()))
            .collect();
        let text = |s: &str| RawValue::Text(s.into());
        let artist = core::str::from_utf8(artist).unwrap();
        assert_eq!(
            tags,
            [
                ("IND", &text("11"), None),
                (
                    "LYR",
                    &text("[00:01]One\r\n[00:02]Two"),
                    Some(StandardTag::Lyrics("[00:01]One\r\n[00:02]Two".into()))
                ),
                (
                    "INF",
                    &text("Caf\u{E9}"),
                    Some(StandardTag::Comment("Caf\u{E9}".into()))
                ),
                (
                    "AUT",
                    &text("Writer"),
                    Some(StandardTag::Lyricist("Writer".into()))
                ),
                (
                    "EAR",
                    &text(artist),
                    Some(StandardTag::Artist(artist.into()))
                ),
                ("ETT", &text("Another title"), None),
                (
                    "EAL",
                    &text("Album"),
                    Some(StandardTag::Album("Album".into()))
                ),
                ("IMG", &text(""), None),
                ("XYZ", &text("data"), None),
            ]
        );

        // A short field may hold 250 bytes, not 251.
        assert!(parse_v2(&lyrics3v2(&[("AUT", &[b'a'; 250])]), &id3v1).is_ok());
        let err =
            parse_v2(&lyrics3v2(&[("INF", b"x"), ("AUT", &[b'a'; 251])]), &id3v1).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidData, Some(20))
        );
        assert!(parse_v2(&lyrics3v2(&[("LYR", &[b'a'; 251])]), &id3v1).is_ok());
    }

    #[test]
    fn malformed_v2() {
        let id3v1 = id3v1_tag(b"", b"", b"");
        let err = |tag: &[u8]| {
            let err = parse_v2(tag, &id3v1).unwrap_err();
            (err.kind, err.byte_offset.unwrap())
        };
        use ErrorKind::{InvalidData, InvalidHeader, UnexpectedEof};

        let good = lyrics3v2(&[("LYR", b"text")]);
        assert_eq!(good.len(), 11 + 12 + 15);
        // No fields at all.
        assert_eq!(err(b"LYRICSBEGIN000011LYRICS200"), (InvalidData, 11));
        // Stored size differs from the tag, or is not six digits.
        let mut wrong_size = good.clone();
        wrong_size[23..29].copy_from_slice(b"000022");
        assert_eq!(err(&wrong_size), (InvalidHeader, 23));
        wrong_size[23..29].copy_from_slice(b"00002x");
        assert_eq!(err(&wrong_size), (InvalidHeader, 23));
        // Magic bytes.
        let mut bad = good.clone();
        bad[0] = b'l';
        assert_eq!(err(&bad), (InvalidHeader, 0));
        let mut bad = good.clone();
        bad[37] = b'1';
        assert_eq!(err(&bad), (InvalidHeader, 29));
        assert_eq!(err(b"LYRICSBEGINLYRICS200"), (InvalidHeader, 0));
        // Field identifier and size.
        let mut bad = good.clone();
        bad[11] = b'l';
        assert_eq!(err(&bad), (InvalidData, 11));
        let mut bad = good.clone();
        bad[18] = b' ';
        assert_eq!(err(&bad), (InvalidData, 11));
        // A field that runs past the size digits.
        let mut bad = good.clone();
        bad[18] = b'5';
        assert_eq!(err(&bad), (UnexpectedEof, 19));
        // A field header cut by the size digits.
        let mut short = BEGIN.to_vec();
        short.extend_from_slice(b"LYR0000");
        short.extend_from_slice(b"000018LYRICS200");
        assert_eq!(err(&short), (UnexpectedEof, 11));
        // The ID3v1 tag must follow.
        let mut not_id3v1 = id3v1;
        not_id3v1[0] = b'X';
        let err = parse_v2(&good, &not_id3v1).unwrap_err();
        assert_eq!((err.kind, err.byte_offset), (InvalidHeader, Some(38)));
    }

    #[test]
    fn find_v1_tag() {
        let find = |file: &[u8], end: u64| {
            let mut reader = ByteReader::new(Cursor::new(file));
            find_v1_start(&mut reader, end).unwrap()
        };
        let lyrics = vec![b'a'; V1_MAX_LYRICS];
        let mut file = b"audio".to_vec();
        file.extend_from_slice(&BEGIN);
        file.extend_from_slice(&lyrics);
        file.extend_from_slice(&V1_END);
        let end = file.len() as u64;
        assert_eq!(find(&file, end), Some(5));
        // 5101 bytes of lyrics are too many.
        file.insert(5 + BEGIN.len(), b'a');
        assert_eq!(find(&file, end + 1), None);
        // The nearest begin marker wins; the tag may start at byte 0.
        let file = b"LYRICSBEGINxLYRICSBEGINyLYRICSEND";
        assert_eq!(find(file, file.len() as u64), Some(12));
        let file = b"LYRICSBEGINLYRICSEND";
        assert_eq!(find(file, file.len() as u64), Some(0));
        // No end marker, or no room for one.
        assert_eq!(find(b"LYRICSBEGINxLYRICSENx", 21), None);
        assert_eq!(find(b"LYRICSEN", 8), None);
        assert_eq!(find(b"xLYRICSEND", 10), None);
    }

    #[test]
    fn read_trailing_v2_tag() {
        let id3v1 = id3v1_tag(b"", b"", b"");
        let tag = lyrics3v2(&[("LYR", b"text")]);
        let mut file = b"audio".to_vec();
        file.extend_from_slice(&tag);
        let end = file.len() as u64;
        file.extend_from_slice(&id3v1);
        let read = |file: &[u8], end: u64, limits: &FormatLimits| {
            let mut reader = ByteReader::new(Cursor::new(file));
            let mut budget = MetadataBudget::new(limits);
            read_trailing_v2(&mut reader, end, 0, &id3v1, &mut budget)
        };

        let found = read(&file, end, &FormatLimits::new()).unwrap().unwrap();
        assert_eq!(found.start, 5);
        assert_eq!(
            found.revision.tags[0].standard,
            Some(StandardTag::Lyrics("text".into()))
        );

        assert_eq!(read(&file, 14, &FormatLimits::new()), Ok(None));
        assert_eq!(read(&file, end - 1, &FormatLimits::new()), Ok(None));
        let limits = FormatLimits::new().with_max_metadata_bytes(tag.len() - 1);
        let err = read(&file, end, &limits).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::LimitExceeded, Some(5))
        );
        let limits = FormatLimits::new().with_max_metadata_bytes(tag.len());
        assert!(read(&file, end, &limits).unwrap().is_some());
        // Not before the floor.
        let mut reader = ByteReader::new(Cursor::new(&file));
        let mut budget = MetadataBudget::new(&FormatLimits::new());
        let err = read_trailing_v2(&mut reader, end, 6, &id3v1, &mut budget).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidHeader);
        assert_eq!(budget, MetadataBudget::new(&FormatLimits::new()));

        // Size digits that are not digits, too small or too large.
        for digits in [&b"0000x8"[..], b"000010", b"000034"] {
            let mut bad = file.clone();
            bad[5 + 23..5 + 29].copy_from_slice(digits);
            let err = read(&bad, end, &FormatLimits::new()).unwrap_err();
            assert_eq!(
                (err.kind, err.byte_offset),
                (ErrorKind::InvalidHeader, Some(28)),
                "{digits:?}"
            );
        }
        // Field errors carry absolute offsets.
        let mut bad = file.clone();
        bad[5 + 11] = b'l';
        let err = read(&bad, end, &FormatLimits::new()).unwrap_err();
        assert_eq!(
            (err.kind, err.byte_offset),
            (ErrorKind::InvalidData, Some(16))
        );
    }
}
