//! The checks without `read_metadata` against the parsers: same result,
//! errors and offsets included, for valid, damaged and truncated tags, any
//! buffer and any cut of the reads; the scanned bytes never exceed the tag.

use super::*;
use crate::ape;
use crate::{Found, id3v1, lyrics3, parsed};
use akousma_core::FormatLimits;
use std::io::{self, Cursor};

/// Buffer sizes tried: tiny ones cut every structure, the others fall
/// around the 256-byte key window and the working size.
const BUFFERS: [usize; 12] = [1, 2, 3, 4, 7, 8, 9, 16, 255, 256, 257, BUFFER_LEN];

/// A source that gives at most `step` bytes per read.
struct Trickle {
    inner: Cursor<Vec<u8>>,
    step: usize,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(self.step);
        self.inner.read(&mut buf[..len])
    }
}

impl Seek for Trickle {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A source whose byte `at` cannot be read: a read that starts there fails
/// (or ends, with `ends`), one that starts before stops there.
struct BadByte<'a> {
    inner: Cursor<&'a [u8]>,
    at: u64,
    ends: bool,
}

impl Read for BadByte<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.inner.position();
        if pos == self.at {
            return if self.ends {
                Ok(0)
            } else {
                Err(io::Error::other("bad sector"))
            };
        }
        let len = match self.at.checked_sub(pos) {
            Some(left) => usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len())),
            None => buf.len(),
        };
        self.inner.read(&mut buf[..len])
    }
}

impl Seek for BadByte<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

const TEXT: u32 = 0;
const BINARY: u32 = 1 << 1;
const LOCATOR: u32 = 2 << 1;
const RESERVED: u32 = 3 << 1;

fn item(key: &[u8], flags: u32, value: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(value.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(key);
    bytes.push(0);
    bytes.extend_from_slice(value);
    bytes
}

/// `"audio"`, the items and an APE footer of `version` for `count` items.
fn ape_file(version: u32, count: u32, items: &[u8]) -> Vec<u8> {
    let mut file = b"audio".to_vec();
    file.extend_from_slice(items);
    file.extend_from_slice(&ape::MAGIC);
    let size = u32::try_from(items.len() + 32).unwrap();
    for field in [version, size, count, 0, 0, 0] {
        file.extend_from_slice(&field.to_le_bytes());
    }
    file
}

/// Result of the parser on the tag that ends `file`, as the scanner sees
/// it, and the frame of the tag.
fn ape_parsed(file: &[u8]) -> (ape::Frame, Result<Found<()>, Error>) {
    let mut reader = ByteReader::new(Cursor::new(file));
    let end = file.len() as u64;
    let Ok(Found::Tag(frame)) = ape::trailing_frame(&mut reader, end, 0) else {
        panic!("no APE frame");
    };
    let start = usize::try_from(frame.items_start).unwrap();
    let items = &file[start..start + usize::try_from(frame.items_len).unwrap()];
    let result = parsed(ape::parse_items_at(&frame.header, items, frame.items_start));
    (frame, result.map(|found| found.map(|_| ())))
}

/// Checks the tag of `frame` in `source` through a buffer of `buffer`
/// bytes; returns the result and the scanned bytes.
fn ape_checked<R: Read + Seek>(
    source: R,
    frame: &ape::Frame,
    buffer: usize,
    limits: &FormatLimits,
) -> (Result<Found<()>, Error>, usize) {
    let mut reader = ByteReader::new(source);
    let mut budget = MetadataBudget::new(limits);
    let mut buf = vec![0; buffer];
    let result = ape::check_frame(&mut reader, frame, &mut budget, &mut buf);
    let scanned = limits.max_tag_scan_bytes - budget.remaining_scan_bytes();
    (result, scanned)
}

/// The check of the tag that ends `file` agrees with the parser for every
/// buffer, with whole and with cut reads, and within exactly the bytes it
/// scans.
fn agree_ape(file: &[u8], label: &str) {
    agree_ape_with(file, label, &BUFFERS, &[usize::MAX, 1, 3]);
}

fn agree_ape_with(file: &[u8], label: &str, buffers: &[usize], steps: &[usize]) {
    let (frame, expected) = ape_parsed(file);
    let full = FormatLimits::new().with_max_tag_scan_bytes(usize::MAX);
    for &buffer in buffers {
        for &step in steps {
            let source = Trickle {
                inner: Cursor::new(file.to_vec()),
                step,
            };
            let (result, scanned) = ape_checked(source, &frame, buffer, &full);
            assert_eq!(result, expected, "{label}: buffer {buffer}, step {step}");
            assert!(
                scanned as u64 <= frame.items_len,
                "{label}: {scanned} bytes"
            );
            if step != usize::MAX {
                continue;
            }
            // Exactly the bytes scanned are needed: one less runs out at the
            // start of the tag.
            let source = Cursor::new(file);
            let enough = FormatLimits::new().with_max_tag_scan_bytes(scanned);
            assert_eq!(ape_checked(source, &frame, buffer, &enough).0, expected);
            if let Some(less) = scanned.checked_sub(1) {
                let short = FormatLimits::new().with_max_tag_scan_bytes(less);
                let (result, _) = ape_checked(Cursor::new(file), &frame, buffer, &short);
                assert_eq!(
                    result,
                    Err(Error::at(ErrorKind::LimitExceeded, frame.start)),
                    "{label}: buffer {buffer}, {less} bytes"
                );
            }
        }
    }
}

/// Items of every type and their edge cases, each well-formed or not.
fn ape_items() -> Vec<(&'static str, Vec<u8>)> {
    let long_text: Vec<u8> = "a\u{E9}\u{20AC}\u{1F3B5}".repeat(700).into_bytes();
    let mut picture = b"cover \xC3\xA9.png\0\x89PNG\r\n\x1A\n".to_vec();
    picture.extend((0..20_000u32).map(|i| (i % 251) as u8));
    let key_255 = vec![b'K'; 255];
    let key_256 = vec![b'K'; 256];
    vec![
        ("text", item(b"Title", TEXT, b"Song")),
        ("values", item(b"Artist", TEXT, b"A\0B\0")),
        ("empty", item(b"Album", TEXT, b"")),
        (
            "multibyte",
            item(b"Comment", TEXT, "Caf\u{E9} \u{20AC} \u{1F3B5}".as_bytes()),
        ),
        ("long text", item(b"Lyrics", TEXT, &long_text)),
        ("bad UTF-8", item(b"Title", TEXT, b"ab\xFFcd")),
        ("cut character", item(b"Title", TEXT, b"ab\xE2\x82")),
        ("overlong", item(b"Title", TEXT, b"\xC0\xAF")),
        ("surrogate", item(b"Title", TEXT, b"\xED\xA0\x80")),
        ("binary", item(b"Data", BINARY, &[0, 1, 2, 0xFF, 0])),
        ("picture", item(b"Cover Art (Front)", BINARY, &picture)),
        (
            "picture case",
            item(b"cover art (back)", BINARY, b"\0\xFF\xD8"),
        ),
        ("picture text", item(b"Cover Art (Front)", TEXT, b"a\0b")),
        (
            "picture without 0",
            item(b"Cover Art (Front)", BINARY, b"abc"),
        ),
        (
            "picture bad name",
            item(b"Cover Art (Front)", BINARY, b"\xC3\0x"),
        ),
        ("locator", item(b"File", LOCATOR, b"a.flac\0b.flac")),
        ("bad locator", item(b"File", LOCATOR, b"\xFE")),
        (
            "picture link",
            item(b"Cover Art (Fish)", LOCATOR, b"http://a\0b"),
        ),
        (
            "bad picture link",
            item(b"Cover Art (Fish)", LOCATOR, b"\x80"),
        ),
        ("reserved", item(b"Odd", RESERVED | 1, b"x\xFF")),
        ("key of 1", item(b"A", TEXT, b"x")),
        ("key of 255", item(&key_255, TEXT, b"x")),
        ("key of 256", item(&key_256, TEXT, b"x")),
        ("forbidden key", item(b"TAG", TEXT, b"x")),
        ("control key", item(b"Ti\x7Fle", TEXT, b"x")),
        ("high key", item(b"Ti\xC3\xA9", TEXT, b"x")),
    ]
}

#[test]
fn ape_items_of_every_kind() {
    for (label, bytes) in ape_items() {
        agree_ape(&ape_file(ape::VERSION_2, 1, &bytes), label);
    }
    // APEv1: every value is text, UTF-8 or else Windows-1252.
    let mut v1 = item(b"Artist", BINARY, b"Caf\xE9 \x93x\x94\0B");
    v1.extend(item(b"Album", TEXT, "Caf\u{E9}".as_bytes()));
    v1.extend(item(b"Cover Art (Front)", BINARY, b"no zero"));
    agree_ape(&ape_file(ape::VERSION_1, 3, &v1), "APEv1");
}

#[test]
fn ape_tags_damaged_and_truncated() {
    let good: Vec<u8> = ape_items()
        .into_iter()
        .filter(|(label, _)| {
            [
                "text",
                "values",
                "binary",
                "locator",
                "picture case",
                "reserved",
            ]
            .contains(label)
        })
        .flat_map(|(_, bytes)| bytes)
        .collect();
    let count = 6;
    agree_ape(&ape_file(ape::VERSION_2, count, &good), "all");
    // More items announced than stored, fewer, none.
    agree_ape(&ape_file(ape::VERSION_2, count + 3, &good), "more");
    agree_ape(&ape_file(ape::VERSION_2, count - 1, &good), "fewer");
    agree_ape(&ape_file(ape::VERSION_2, 0, &good), "none");
    agree_ape(&ape_file(ape::VERSION_2, 3, &[]), "empty");
    let mut extra = good.clone();
    extra.push(0);
    agree_ape(&ape_file(ape::VERSION_2, count, &extra), "extra byte");

    // Every truncation of the items, the tag size following it.
    for len in 0..good.len() {
        let file = ape_file(ape::VERSION_2, count, &good[..len]);
        agree_ape_with(
            &file,
            &format!("cut at {len}"),
            &[1, 9, 256, BUFFER_LEN],
            &[usize::MAX],
        );
    }
    // Every byte of the items replaced by values that break the rules.
    for pos in 0..good.len() {
        for byte in [0x00, 0x01, 0x20, 0x7F, 0x80, 0xFF, good[pos] ^ 1] {
            let mut bad = good.clone();
            bad[pos] = byte;
            let file = ape_file(ape::VERSION_2, count, &bad);
            agree_ape_with(
                &file,
                &format!("{byte:#x} at {pos}"),
                &[1, 7, BUFFER_LEN],
                &[usize::MAX],
            );
        }
    }
}

#[test]
fn ape_limits_of_the_parser() {
    // 65 536 items are accepted, one more is not: in both, at the value of
    // the item that does not fit.
    let one = item(b"K1", BINARY, b"");
    let file = ape_file(ape::VERSION_2, 65_536, &one.repeat(65_536));
    agree_ape_with(&file, "65 536 items", &[7, BUFFER_LEN], &[usize::MAX]);
    let file = ape_file(ape::VERSION_2, 65_537, &one.repeat(65_537));
    let (_, expected) = ape_parsed(&file);
    assert_eq!(
        expected.unwrap_err().kind,
        ErrorKind::LimitExceeded,
        "the parser must refuse the item"
    );
    agree_ape_with(&file, "65 537 items", &[7, BUFFER_LEN], &[usize::MAX]);

    // 65 536 values in one item, and one more; a picture link is not split.
    for (key, flags) in [
        (&b"Artist"[..], TEXT),
        (b"File", LOCATOR),
        (b"Cover Art (Fish)", LOCATOR),
    ] {
        for zeros in [65_535, 65_536] {
            let file = ape_file(ape::VERSION_2, 1, &item(key, flags, &vec![0; zeros]));
            agree_ape_with(
                &file,
                &format!("{zeros} zeros"),
                &[1, BUFFER_LEN],
                &[usize::MAX],
            );
        }
    }
    // Invalid UTF-8 wins over the number of values, as in the parser.
    let mut text = vec![0; 65_536];
    text.push(0xFF);
    let file = ape_file(ape::VERSION_2, 1, &item(b"Artist", TEXT, &text));
    let (_, expected) = ape_parsed(&file);
    assert!(matches!(expected, Ok(Found::Damaged(_))));
    agree_ape_with(&file, "zeros and bad UTF-8", &[BUFFER_LEN], &[usize::MAX]);
}

#[test]
fn ape_source_failing_anywhere() {
    // A picture between two text items: the image is skipped, the rest is
    // read, and a source that fails in what is read is an error.
    let mut items = item(b"Title", TEXT, b"Song");
    let picture_start = 5 + items.len() as u64;
    let mut picture = b"front\0".to_vec();
    picture.resize(BUFFER_LEN + 3000, 0xAB);
    items.extend(item(b"Cover Art (Front)", BINARY, &picture));
    let picture_end = 5 + items.len() as u64;
    items.extend(item(b"Artist", TEXT, b"Band"));
    let file = ape_file(ape::VERSION_2, 3, &items);
    let (frame, expected) = ape_parsed(&file);
    assert_eq!(expected, Ok(Found::Tag(())));
    let full = FormatLimits::new();
    for buffer in [16, BUFFER_LEN] {
        let mut skipped = 0;
        for at in frame.items_start..frame.items_start + frame.items_len {
            for ends in [false, true] {
                let source = BadByte {
                    inner: Cursor::new(&file),
                    at,
                    ends,
                };
                let (result, _) = ape_checked(source, &frame, buffer, &full);
                match result {
                    Ok(found) => {
                        // Only bytes of the image that were not read.
                        assert_eq!(found, Found::Tag(()), "{at}");
                        assert!((picture_start + 32..picture_end).contains(&at), "{at}");
                        skipped += 1;
                    }
                    Err(err) => {
                        let kind = if ends {
                            ErrorKind::UnexpectedEof
                        } else {
                            ErrorKind::Io(akousma_core::IoErrorKind::Other)
                        };
                        assert_eq!(err.kind, kind, "{at}");
                    }
                }
            }
        }
        assert!(
            skipped > 0,
            "the image is skipped with a buffer of {buffer}"
        );
    }
}

/// `"audio"`, a Lyrics3 v2.00 tag of `body` (from `BEGIN` to the last
/// field) and an ID3v1 tag; returns the file and the end of the tag.
fn lyrics_file(body: &[u8]) -> (Vec<u8>, u64) {
    let mut file = b"audio".to_vec();
    file.extend_from_slice(body);
    file.extend_from_slice(format!("{:06}", body.len()).as_bytes());
    file.extend_from_slice(&lyrics3::V2_END);
    let end = file.len() as u64;
    file.extend_from_slice(&id3v1_tag());
    (file, end)
}

fn id3v1_tag() -> [u8; id3v1::TAG_LEN] {
    let mut tag = [0; id3v1::TAG_LEN];
    tag[..3].copy_from_slice(&id3v1::MAGIC);
    tag
}

fn body(fields: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut body = lyrics3::BEGIN.to_vec();
    for (id, data) in fields {
        body.extend_from_slice(id);
        body.extend_from_slice(format!("{:05}", data.len()).as_bytes());
        body.extend_from_slice(data);
    }
    body
}

/// The check of the Lyrics3 tag that ends at `end` agrees with the parser,
/// as in [`agree_ape`]. A footer that does not locate a tag is skipped.
fn agree_lyrics(
    file: &[u8],
    end: u64,
    id3v1: &[u8; id3v1::TAG_LEN],
    label: &str,
    buffers: &[usize],
) {
    let mut reader = ByteReader::new(Cursor::new(file));
    let Ok(Found::Tag(start)) = lyrics3::v2_start(&mut reader, end, 0) else {
        return;
    };
    let tag = &file[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
    let expected = parsed(lyrics3::parse_v2_at(tag, id3v1, start)).map(|found| found.map(|_| ()));
    let check = |source: Trickle, buffer: usize, limit: usize| {
        let mut reader = ByteReader::new(source);
        let mut budget = MetadataBudget::new(&FormatLimits::new().with_max_tag_scan_bytes(limit));
        let mut buf = vec![0; buffer];
        let result = lyrics3::check_v2(&mut reader, start, end, id3v1, &mut budget, &mut buf);
        (result, limit - budget.remaining_scan_bytes())
    };
    let source = |step| Trickle {
        inner: Cursor::new(file.to_vec()),
        step,
    };
    for &buffer in buffers {
        for step in [usize::MAX, 1, 3] {
            let (result, scanned) = check(source(step), buffer, usize::MAX);
            assert_eq!(result, expected, "{label}: buffer {buffer}, step {step}");
            // The footer is not read again.
            assert!(scanned as u64 <= end - start - 15, "{label}");
            if step == usize::MAX {
                assert_eq!(check(source(step), buffer, scanned).0, expected, "{label}");
                if let Some(less) = scanned.checked_sub(1) {
                    let limit = Err(Error::at(ErrorKind::LimitExceeded, start));
                    assert_eq!(check(source(step), buffer, less).0, limit, "{label}");
                }
            }
        }
    }
}

#[test]
fn lyrics3_tags_valid_damaged_and_truncated() {
    let lyrics = "[00:01]Caf\u{E9}\r\n".repeat(900);
    let good = body(&[
        (b"IND", b"11"),
        (b"LYR", lyrics.as_bytes()),
        (b"INF", b"\xE9"),
        (b"AUT", &[b'a'; 250]),
        (b"EAL", b""),
        (b"XYZ", b"data"),
    ]);
    let id3v1 = id3v1_tag();
    let (file, end) = lyrics_file(&good);
    agree_lyrics(&file, end, &id3v1, "good", &BUFFERS);
    // The ID3v1 tag must start with its magic.
    let mut not_id3v1 = id3v1;
    not_id3v1[0] = b'X';
    agree_lyrics(&file, end, &not_id3v1, "no ID3v1", &[1, BUFFER_LEN]);

    // Single rules, each broken once.
    let cases: [(&str, Vec<u8>); 7] = [
        ("no fields", lyrics3::BEGIN.to_vec()),
        ("long AUT", body(&[(b"INF", b"x"), (b"AUT", &[b'a'; 251])])),
        ("long ETT", body(&[(b"ETT", &[b'a'; 300])])),
        ("long LYR", body(&[(b"LYR", &[b'a'; 300])])),
        ("lower id", body(&[(b"lYR", b"x")])),
        ("bad digits", [&lyrics3::BEGIN[..], b"LYR0000x"].concat()),
        ("cut header", [&lyrics3::BEGIN[..], b"LYR0000"].concat()),
    ];
    for (label, body) in cases {
        let (file, end) = lyrics_file(&body);
        agree_lyrics(&file, end, &id3v1, label, &BUFFERS);
    }
    let mut bad_begin = good.clone();
    bad_begin[0] = b'l';
    let (file, end) = lyrics_file(&bad_begin);
    agree_lyrics(&file, end, &id3v1, "bad begin", &[1, BUFFER_LEN]);

    // Every truncation of the fields, the stored size following it.
    let small = body(&[(b"IND", b"11"), (b"LYR", b"la la"), (b"AUT", b"Me")]);
    for len in 0..=small.len() {
        let (file, end) = lyrics_file(&small[..len]);
        agree_lyrics(
            &file,
            end,
            &id3v1,
            &format!("cut at {len}"),
            &[1, 9, BUFFER_LEN],
        );
    }
    // Every byte of the tag, footer included, replaced.
    let (file, end) = lyrics_file(&small);
    for pos in 5..usize::try_from(end).unwrap() {
        for byte in [0x00, b'0', b'9', b'A', b'a', 0xFF, file[pos] ^ 1] {
            let mut bad = file.clone();
            bad[pos] = byte;
            agree_lyrics(
                &bad,
                end,
                &id3v1,
                &format!("{byte:#x} at {pos}"),
                &[1, 7, BUFFER_LEN],
            );
        }
    }
}

#[test]
fn lyrics3_field_limit() {
    let id3v1 = id3v1_tag();
    for fields in [65_536, 65_537] {
        let mut body = lyrics3::BEGIN.to_vec();
        body.extend(b"IND00000".repeat(fields));
        let (file, end) = lyrics_file(&body);
        agree_lyrics(
            &file,
            end,
            &id3v1,
            &format!("{fields} fields"),
            &[7, BUFFER_LEN],
        );
    }
}

#[test]
fn utf8_in_pieces() {
    let samples: [&[u8]; 12] = [
        b"",
        b"plain",
        "\u{E9}\u{20AC}\u{1F3B5}x".as_bytes(),
        b"\xE2\x82",
        b"\xF0\x9F\x8E",
        b"a\xFFb",
        b"\xC0\xAF",
        b"\xED\xA0\x80",
        b"\xF4\x90\x80\x80",
        b"\xE2\x82\xE2\x82\xAC",
        b"\x80",
        b"\xF0\x9F\x8E\xB5\xF0",
    ];
    for sample in samples {
        let expected = core::str::from_utf8(sample).is_ok();
        // Cut once anywhere, and in pieces of every small size.
        for cut in 0..=sample.len() {
            let mut check = Utf8Check::new();
            check.push(&sample[..cut]);
            check.push(&sample[cut..]);
            assert_eq!(check.is_valid(), expected, "{sample:?} cut at {cut}");
        }
        for size in 1..=5 {
            let mut check = Utf8Check::new();
            for piece in sample.chunks(size) {
                check.push(piece);
                check.push(&[]);
            }
            assert_eq!(check.is_valid(), expected, "{sample:?} in pieces of {size}");
        }
    }
}

#[test]
fn stream_reads_once_and_skips_by_seeking() {
    let data: Vec<u8> = (0..100).collect();
    let mut reader = ByteReader::new(Cursor::new(&data));
    let mut budget = MetadataBudget::new(&FormatLimits::new().with_max_tag_scan_bytes(1000));
    let mut buf = [0; 8];
    let mut stream = TagStream::new(&mut reader, &mut budget, 3, 10, 50, &mut buf);
    let mut two = [0; 2];
    stream.read_exact(&mut two).unwrap();
    assert_eq!(two, [10, 11]);
    // The rest of the buffer is used up, then 20 bytes are not read.
    stream.skip(26);
    assert_eq!(stream.position(), 38);
    assert_eq!(stream.peek(100).unwrap(), [38, 39, 40, 41, 42, 43, 44, 45]);
    stream.consume(8);
    // Read-ahead never goes past the end of the range.
    assert_eq!(stream.peek(100).unwrap(), [46, 47, 48, 49]);
    stream.consume(4);
    assert_eq!((stream.remaining(), stream.peek(1).unwrap()), (0, &[][..]));
    stream.skip(5);
    assert_eq!(stream.position(), 50);
    assert_eq!(budget.remaining_scan_bytes(), 1000 - 8 - 8 - 4);
}
