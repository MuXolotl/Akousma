//! A source that ends inside the length it reported, or fails, is an error
//! of the scan; a tag whose bytes were all read and are malformed is only
//! left out.

use super::*;
use std::io::{self, SeekFrom};
use std::ops::Range;

/// A source over `data` whose reads that start inside `gap` end the data
/// early (`Ok(0)`), or fail if `fail` is set. Reads that start before the
/// gap stop at its start.
struct Faulty {
    inner: Cursor<Vec<u8>>,
    gap: Range<u64>,
    fail: bool,
}

impl Read for Faulty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.inner.position();
        if self.gap.contains(&pos) {
            return if self.fail {
                Err(io::Error::other("bad sector"))
            } else {
                Ok(0)
            };
        }
        let room = if pos < self.gap.start {
            usize::try_from(self.gap.start - pos).unwrap_or(usize::MAX)
        } else {
            usize::MAX
        };
        let len = buf.len().min(room);
        self.inner.read(&mut buf[..len])
    }
}

impl Seek for Faulty {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// Both values of `read_metadata`.
fn both_modes() -> [DemuxOptions; 2] {
    [
        DemuxOptions::new(),
        DemuxOptions::new().with_read_metadata(false),
    ]
}

/// Scans the end of `file` with no floor, through a source with `gap`.
fn trailing(
    file: &[u8],
    gap: Range<u64>,
    fail: bool,
    options: &DemuxOptions,
) -> Result<Anchors, Error> {
    let source = Faulty {
        inner: Cursor::new(file.to_vec()),
        gap,
        fail,
    };
    let mut reader = ByteReader::new(source);
    let mut anchors = Anchors {
        audio_start: 0,
        audio_end: file.len() as u64,
        ..Anchors::default()
    };
    let mut budget = MetadataBudget::new(&options.limits);
    scan_trailing(&mut reader, &mut anchors, 0, options, &mut budget)?;
    Ok(anchors)
}

/// Scans the start of `data`, whose source reports `len` bytes.
fn leading(data: &[u8], len: u64, options: &DemuxOptions) -> Result<Anchors, Error> {
    let mut reader = ByteReader::new(Cursor::new(data));
    let mut budget = MetadataBudget::new(&options.limits);
    scan_leading(&mut reader, len, options, &mut budget)
}

/// The kind and offset of an error.
fn failure(result: Result<Anchors, Error>) -> (ErrorKind, Option<u64>) {
    let err = result.unwrap_err();
    (err.kind, err.byte_offset)
}

const EOF: ErrorKind = ErrorKind::UnexpectedEof;
const IO: ErrorKind = ErrorKind::Io(akousma_core::IoErrorKind::Other);

#[test]
fn trailing_ape_cut_by_the_source() {
    // audio (5) | header (32) | item (17) | footer (32)
    let item = ape_item("Title", "Ape");
    assert_eq!(item.len(), 17);
    let file = [b"audio".to_vec(), ape_tag(&item, Some(0), true)].concat();
    let items = 5 + 32;
    for options in both_modes() {
        let clean = trailing(&file, 0..0, false, &options).unwrap();
        assert_eq!(clean.audio_end, 5);
        // The items, then the header, end early or fail.
        for (gap, at) in [(items..items + 1, items), (5..6, 5)] {
            assert_eq!(
                failure(trailing(&file, gap.clone(), false, &options)),
                (EOF, Some(at))
            );
            assert_eq!(
                failure(trailing(&file, gap, true, &options)),
                (IO, Some(at))
            );
        }
    }

    // The same tag with an item that runs 1 byte past the end of the
    // items: all of its bytes are read, it is malformed, and it is left as
    // audio.
    let mut cut = file.clone();
    let value_len = u32::from_le_bytes(cut[items as usize..][..4].try_into().unwrap());
    cut[items as usize..][..4].copy_from_slice(&(value_len + 1).to_le_bytes());
    for options in both_modes() {
        let anchors = trailing(&cut, 0..0, false, &options).unwrap();
        assert_eq!((anchors.audio_end, anchors.trailing.len()), (86, 0));
    }
}

#[test]
fn leading_ape_cut_by_the_source() {
    // header (32) | item (17) | footer (32) | audio
    let file = [
        ape_tag(&ape_item("Title", "Ape"), Some(0), true),
        b"audio".to_vec(),
    ]
    .concat();
    let len = file.len() as u64;
    for options in both_modes() {
        assert_eq!(leading(&file, len, &options).unwrap().audio_start, 81);
        // The source reports the whole length but its data stop inside the
        // items: the footer at 49 cannot be read.
        assert_eq!(
            failure(leading(&file[..40], len, &options)),
            (EOF, Some(49))
        );
        // The data stop inside the footer.
        assert_eq!(
            failure(leading(&file[..60], len, &options)),
            (EOF, Some(49))
        );
    }

    // An item that runs past the end of the items, with all bytes there.
    let mut cut = file.clone();
    cut[32..36].copy_from_slice(&4u32.to_le_bytes());
    for options in both_modes() {
        let anchors = leading(&cut, len, &options).unwrap();
        assert_eq!((anchors.audio_start, anchors.leading.len()), (0, 0));
    }
}

#[test]
fn lyrics3_cut_by_the_source() {
    // audio (20) | Lyrics3 v2.00 (39) | ID3v1 (128)
    let audio = [0x55u8; 20];
    let lyrics = lyrics3v2_tag();
    assert_eq!(lyrics.len(), 39);
    let file = [audio.to_vec(), lyrics, id3v1_tag("V1")].concat();
    for options in both_modes() {
        assert_eq!(
            trailing(&file, 0..0, false, &options).unwrap().audio_end,
            20
        );
        // The body of the tag, after its footer was read.
        assert_eq!(
            failure(trailing(&file, 20..21, false, &options)),
            (EOF, Some(20))
        );
        assert_eq!(
            failure(trailing(&file, 20..21, true, &options)),
            (IO, Some(20))
        );
    }
    // A field longer than the tag: malformed, left as audio; the ID3v1 tag
    // is still taken.
    let mut cut = file.clone();
    cut[20 + 14..20 + 19].copy_from_slice(b"00099");
    for options in both_modes() {
        let anchors = trailing(&cut, 0..0, false, &options).unwrap();
        assert_eq!(anchors.audio_end, 59);
    }

    // audio (20) | Lyrics3 v1.00 | ID3v1: the search window starts at 0.
    let file = [
        audio.to_vec(),
        b"LYRICSBEGINla laLYRICSEND".to_vec(),
        id3v1_tag("V1"),
    ]
    .concat();
    for options in both_modes() {
        assert_eq!(
            trailing(&file, 0..0, false, &options).unwrap().audio_end,
            20
        );
        assert_eq!(
            failure(trailing(&file, 0..1, false, &options)),
            (EOF, Some(0))
        );
        assert_eq!(
            failure(trailing(&file, 0..1, true, &options)),
            (IO, Some(0))
        );
    }
}
