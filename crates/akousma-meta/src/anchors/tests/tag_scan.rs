//! The scan without `read_metadata` against the scan with it: a scan that
//! succeeds with metadata succeeds without it, with the same audio range,
//! as long as `max_tag_scan_bytes` is not smaller than
//! `max_metadata_bytes`; rejected tags and tags read twice included.

use super::*;
use crate::stream::BUFFER_LEN;

/// Limits of `metadata` bytes and `scan` scanned bytes.
fn options(metadata: usize, scan: usize, read_metadata: bool) -> DemuxOptions {
    let limits = FormatLimits::new()
        .with_max_metadata_bytes(metadata)
        .with_max_tag_scan_bytes(scan);
    DemuxOptions::new()
        .with_limits(limits)
        .with_read_metadata(read_metadata)
}

/// The audio range found in `file`, scanning it `times` times with one
/// budget.
fn range(file: &[u8], options: &DemuxOptions, times: usize) -> Result<(u64, u64), Error> {
    let mut reader = ByteReader::new(Cursor::new(file));
    let mut budget = MetadataBudget::new(&options.limits);
    let mut found = (0, 0);
    for _ in 0..times {
        let anchors = scan(&mut reader, file.len() as u64, options, &mut budget)?;
        found = (anchors.audio_start, anchors.audio_end);
    }
    Ok(found)
}

/// Checks the guarantee for `file` with every limit in `limits`, and returns
/// how many of them let the scan with metadata succeed.
fn guarantee(file: &[u8], limits: impl IntoIterator<Item = usize>, label: &str) -> usize {
    let limits: Vec<usize> = limits.into_iter().collect();
    let mut opened = 0;
    for times in [1, 2] {
        for &limit in &limits {
            let Ok(with) = range(file, &options(limit, limit, true), times) else {
                continue;
            };
            opened += 1;
            for scan in [limit, limit.saturating_add(7), usize::MAX] {
                let without = range(file, &options(limit, scan, false), times);
                assert_eq!(
                    without,
                    Ok(with),
                    "{label}: limit {limit}, scan {scan}, {times}x"
                );
            }
        }
    }
    opened
}

/// An APEv2 tag with a header and one text item, and a damaged one whose
/// key is too short.
fn ape_pair(value: &str) -> (Vec<u8>, Vec<u8>) {
    let good = ape_tag(&ape_item("Title", value), Some(0), true);
    let bad = ape_tag(&ape_item("T", value), Some(0), true);
    (good, bad)
}

/// A Lyrics3 v2.00 tag with a field of `len` bytes, and a damaged one whose
/// `AUT` field is too long.
fn lyrics(len: usize) -> (Vec<u8>, Vec<u8>) {
    let tag = |id: &str| {
        let mut tag = format!("LYRICSBEGIN{id}{len:05}").into_bytes();
        tag.resize(tag.len() + len, b'a');
        let size = format!("{:06}", tag.len());
        tag.extend_from_slice(size.as_bytes());
        tag.extend_from_slice(b"LYRICS200");
        tag
    };
    (tag("LYR"), tag("AUT"))
}

#[test]
fn guarantee_for_every_limit() {
    let id3 = b"ID3\x03\x00\x00\x00\x00\x00\x0cTIT2\x00\x00\x00\x02\x00\x00\x00L".to_vec();
    let (lead, bad_lead) = ape_pair("Lead");
    let (tail, bad_tail) = ape_pair("Tail");
    let (lyr, bad_lyr) = lyrics(300);
    let mut extension = vec![0u8; id3v1::EXTENDED_TAG_LEN];
    extension[..4].copy_from_slice(b"TAG+");
    let audio = b"some audio data".to_vec();
    let v1 = id3v1_tag("V1");
    let files: [(&str, Vec<Vec<u8>>); 9] = [
        (
            "tags at both ends",
            vec![
                id3.clone(),
                lead.clone(),
                audio.clone(),
                tail.clone(),
                v1.clone(),
            ],
        ),
        (
            "rejected leading APE",
            vec![bad_lead.clone(), audio.clone(), v1.clone()],
        ),
        (
            "rejected trailing APE",
            vec![audio.clone(), bad_tail.clone()],
        ),
        (
            "APE after ID3v1",
            vec![audio.clone(), v1.clone(), tail.clone()],
        ),
        ("Lyrics3", vec![audio.clone(), lyr.clone(), v1.clone()]),
        (
            "rejected Lyrics3",
            vec![audio.clone(), bad_lyr.clone(), v1.clone()],
        ),
        (
            "APE, Lyrics3, TAG+",
            vec![
                audio.clone(),
                tail.clone(),
                lyr.clone(),
                extension.clone(),
                v1.clone(),
            ],
        ),
        (
            "Lyrics3 v1.00",
            vec![
                audio.clone(),
                b"LYRICSBEGINla laLYRICSEND".to_vec(),
                v1.clone(),
            ],
        ),
        (
            "orphan footer",
            vec![audio.clone(), ape_structure(4000, 1, 0)],
        ),
    ];
    // Where the audio is, without limits: rejected tags stay audio.
    let audio_len = audio.len() as u64;
    let lead_end = (id3.len() + lead.len()) as u64;
    let expected = [
        (lead_end, lead_end + audio_len),
        (0, (bad_lead.len() + audio.len()) as u64),
        (0, audio_len + bad_tail.len() as u64),
        (0, audio_len),
        (0, audio_len),
        (0, audio_len + bad_lyr.len() as u64),
        (0, audio_len),
        (0, audio_len),
        (0, audio_len + 32),
    ];
    for ((label, parts), expected) in files.into_iter().zip(expected) {
        let file = parts.concat();
        for read_metadata in [true, false] {
            let unlimited = options(usize::MAX, usize::MAX, read_metadata);
            assert_eq!(range(&file, &unlimited, 1), Ok(expected), "{label}");
        }
        // Every limit up to the whole file: the tags fit or do not, one by
        // one.
        let opened = guarantee(&file, 0..=file.len() + 1, label);
        assert!(opened > 0, "{label} never opens");
    }
}

#[test]
fn guarantee_for_tags_larger_than_the_buffer() {
    // A picture larger than the buffer, before an ID3v1 tag, and a rejected
    // tag of the same size: the limits around their stored sizes.
    let mut picture = b"front\0".to_vec();
    picture.resize(3 * BUFFER_LEN, 0xAB);
    let mut item = u32::try_from(picture.len()).unwrap().to_le_bytes().to_vec();
    item.extend_from_slice(&(1u32 << 1).to_le_bytes());
    item.extend_from_slice(b"Cover Art (Front)\0");
    item.extend_from_slice(&picture);
    let good = ape_tag(&item, None, true);
    let mut bad_item = item.clone();
    bad_item[8] = 0x7F;
    let bad = ape_tag(&bad_item, None, true);
    let (lyr, _) = lyrics(2 * BUFFER_LEN);
    for (label, file) in [
        (
            "picture",
            [b"audio".to_vec(), good.clone(), id3v1_tag("V1")].concat(),
        ),
        (
            "rejected picture",
            [b"audio".to_vec(), bad, id3v1_tag("V1")].concat(),
        ),
        (
            "long lyrics",
            [b"audio".to_vec(), lyr.clone(), id3v1_tag("V1")].concat(),
        ),
    ] {
        let sizes = [
            0,
            128,
            good.len(),
            good.len() + 128,
            lyr.len(),
            lyr.len() + 128,
        ];
        let limits = sizes
            .into_iter()
            .flat_map(|size| [size.saturating_sub(1), size, size + 1]);
        let opened = guarantee(&file, limits, label);
        assert!(opened > 0, "{label} never opens");
    }
}

#[test]
fn rejected_tag_read_twice() {
    // An ID3v1 tag whose last 32 bytes are an APE footer: the APE tag it
    // announces covers the Lyrics3 tag, is checked and rejected, then the
    // Lyrics3 tag before the ID3v1 tag is read: its bytes are read twice, in
    // both modes.
    let (lyr, _) = lyrics(40);
    let mut v1 = id3v1_tag("V1");
    let size = u32::try_from(lyr.len() + 128).unwrap();
    v1[96..].copy_from_slice(&ape_structure(size, 1, 0));
    let file = [b"audio".to_vec(), lyr.clone(), v1].concat();
    let quiet = range(&file, &options(0, usize::MAX, false), 1);
    assert_eq!(quiet, Ok((0, 5)));
    // With metadata the stored sizes of both tags count, more than the file.
    let opened = guarantee(&file, 0..=2 * file.len(), "read twice");
    assert!(opened > 0);
    // Without metadata both reads count too: the items of the rejected APE
    // tag (all but its footer), then the Lyrics3 tag but its footer, read
    // ahead in one piece.
    let scanned = (lyr.len() + 96) + (lyr.len() - 15);
    assert_eq!(range(&file, &options(0, scanned, false), 1), Ok((0, 5)));
    let err = range(&file, &options(0, scanned - 1, false), 1).unwrap_err();
    assert_eq!(
        (err.kind, err.byte_offset),
        (ErrorKind::LimitExceeded, Some(5))
    );
}

#[test]
fn fixed_reads_are_not_scanned_bytes() {
    // ID3v2 in front; an APE tag without items, TAG+, Lyrics3 v1.00 and
    // ID3v1 after: nothing is checked item by item.
    let id3 = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec();
    let empty_ape = ape_structure(32, 0, 0);
    let mut extension = vec![0u8; id3v1::EXTENDED_TAG_LEN];
    extension[..4].copy_from_slice(b"TAG+");
    let file = [
        id3,
        b"audio".to_vec(),
        b"LYRICSBEGINla laLYRICSEND".to_vec(),
        extension,
        id3v1_tag("V1"),
        empty_ape,
    ]
    .concat();
    assert_eq!(range(&file, &options(0, 0, false), 1), Ok((10, 15)));

    // A tag with items needs scanned bytes without metadata, and none with.
    let (tail, _) = ape_pair("Tail");
    let file = [b"audio".to_vec(), tail].concat();
    let err = range(&file, &options(usize::MAX, 0, false), 1).unwrap_err();
    assert_eq!(
        (err.kind, err.byte_offset),
        (ErrorKind::LimitExceeded, Some(5))
    );
    assert_eq!(range(&file, &options(usize::MAX, 0, true), 1), Ok((0, 5)));
}

#[test]
fn too_many_items_in_both_modes() {
    let one = {
        let mut item = 0u32.to_le_bytes().to_vec();
        item.extend_from_slice(&(1u32 << 1).to_le_bytes());
        item.extend_from_slice(b"K1\0");
        item
    };
    for (count, fits) in [(65_536, true), (65_537, false)] {
        let items = one.repeat(count);
        let mut file = b"audio".to_vec();
        let size = u32::try_from(items.len() + 32).unwrap();
        file.extend(items);
        file.extend(ape_structure(size, u32::try_from(count).unwrap(), 0));
        let with = range(&file, &options(usize::MAX, usize::MAX, true), 1);
        let without = range(&file, &options(usize::MAX, usize::MAX, false), 1);
        if fits {
            assert_eq!((with, without), (Ok((0, 5)), Ok((0, 5))));
        } else {
            let offset = 5 + one.len() as u64 * 65_536 + 11;
            let err = Err(Error::at(ErrorKind::LimitExceeded, offset));
            assert_eq!((&with, &without), (&err, &err));
        }
    }
}
