//! Container bounds, empty `data` chunks, packet and track limits, and the
//! metadata budgets, on hostile and unusual files.

use std::time::Duration;

use akousma_core::codec::Packet;
use akousma_core::meta::RawValue;
use akousma_core::probe::Demuxer;
use akousma_core::{DemuxOptions, ErrorKind, FormatLimits};

use crate::common::{chunk, chunk_be, drain, fmt, open, open_with, riff, wave};
use crate::containers::{ds64, raw_chunk, w64, w64_chunk};

/// Mono 16-bit samples 1 and 2.
const PCM: [u8; 4] = [1, 0, 2, 0];

/// `fmt ` chunk of mono 16-bit PCM at 8 kHz.
fn fmt16() -> Vec<u8> {
    chunk(b"fmt ", &fmt(1, 1, 8000, 2, 16))
}

/// The same for RIFX.
fn fmt16_be() -> Vec<u8> {
    let mut fields = Vec::new();
    for field in [1u16, 1] {
        fields.extend_from_slice(&field.to_be_bytes());
    }
    fields.extend_from_slice(&8000u32.to_be_bytes());
    fields.extend_from_slice(&16_000u32.to_be_bytes());
    fields.extend_from_slice(&2u16.to_be_bytes());
    fields.extend_from_slice(&16u16.to_be_bytes());
    chunk_be(b"fmt ", &fields)
}

/// The `fmt ` payload of Wave64 tests, 18 bytes.
fn fmt18() -> Vec<u8> {
    let mut out = fmt(1, 1, 8000, 2, 16);
    out.extend_from_slice(&[0, 0]);
    out
}

/// A `LIST` of type `INFO` with an `INAM` field.
fn info(title: &str) -> Vec<u8> {
    info_with(title, chunk)
}

fn info_with(title: &str, chunk: fn(&[u8; 4], &[u8]) -> Vec<u8>) -> Vec<u8> {
    let mut value = title.as_bytes().to_vec();
    value.push(0);
    let mut list = b"INFO".to_vec();
    list.extend_from_slice(&chunk(b"INAM", &value));
    chunk(b"LIST", &list)
}

/// A `bext` payload with a description.
fn bext(description: &str) -> Vec<u8> {
    let mut out = vec![0; 602];
    out[..description.len()].copy_from_slice(description.as_bytes());
    out
}

fn meta(read: bool) -> DemuxOptions {
    DemuxOptions::new().with_read_metadata(read)
}

/// Text values of the fields named `key`, in the order of the log.
fn texts(demuxer: &mut dyn Demuxer, key: &str) -> Vec<String> {
    demuxer
        .metadata()
        .iter()
        .flat_map(|revision| &revision.tags)
        .filter(|tag| tag.raw.key == key)
        .filter_map(|tag| match &tag.raw.value {
            RawValue::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The `INAM` and `bext` descriptions.
fn titles(demuxer: &mut dyn Demuxer) -> Vec<String> {
    let mut out = texts(demuxer, "INAM");
    out.extend(texts(demuxer, "BEXT:DESCRIPTION"));
    out
}

/// Checks the samples of `file` with and without metadata, and the titles
/// read with it.
fn check(file: &[u8], samples: &[i16], with: &[&str]) {
    let mut demuxer = open_with(file.to_vec(), &meta(true)).unwrap();
    assert_eq!(titles(&mut demuxer), with);
    assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0], samples);
    let mut demuxer = open_with(file.to_vec(), &meta(false)).unwrap();
    assert!(demuxer.metadata().is_empty());
    assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0], samples);
}

/// The error of opening `file`, with its offset.
fn failure(file: Vec<u8>) -> (ErrorKind, Option<u64>) {
    let err = open(file).unwrap_err();
    (err.kind, err.byte_offset)
}

fn set_size32(file: &mut [u8], size: u32) {
    file[4..8].copy_from_slice(&size.to_le_bytes());
}

/// Sets the RIFF size of the `ds64` chunk that directly follows the stream
/// header.
fn set_ds64_riff_size(file: &mut [u8], size: u64) {
    assert_eq!(&file[12..16], b"ds64");
    file[20..28].copy_from_slice(&size.to_le_bytes());
}

#[test]
fn root_size_bounds_the_container() {
    // RIFF: a list inside the root is read, one after it is not.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM), info("Inside")]);
    file.extend_from_slice(&info("Outside"));
    check(&file, &[1, 2], &["Inside"]);

    // RIFX, with big-endian sizes in the list too.
    let mut file = wave(
        b"RIFX",
        &[
            fmt16_be(),
            chunk_be(b"data", &[0, 1, 0, 2]),
            info_with("Inside", chunk_be),
        ],
    );
    file.extend_from_slice(&info_with("Outside", chunk_be));
    check(&file, &[1, 2], &["Inside"]);

    // RF64 and BW64 with a 32-bit root size.
    for magic in [b"RF64", b"BW64"] {
        let mut file = wave(
            magic,
            &[
                chunk(b"ds64", &ds64(4, 0)),
                fmt16(),
                chunk(b"data", &PCM),
                info("Inside"),
            ],
        );
        file.extend_from_slice(&info("Outside"));
        check(&file, &[1, 2], &["Inside"]);
    }

    // RF64 with the root size in `ds64`; 0 there declares no bound.
    let mut file = wave(
        b"RF64",
        &[
            chunk(b"ds64", &ds64(4, 0)),
            fmt16(),
            raw_chunk(b"data", u32::MAX, &PCM),
            info("Inside"),
        ],
    );
    let size = file.len() as u64 - 8;
    set_ds64_riff_size(&mut file, size);
    set_size32(&mut file, u32::MAX);
    file.extend_from_slice(&info("Outside"));
    check(&file, &[1, 2], &["Inside"]);
    set_ds64_riff_size(&mut file, 0);
    check(&file, &[1, 2], &["Inside", "Outside"]);

    // Wave64.
    let mut file = w64(&[
        w64_chunk(b"fmt ", &fmt18()),
        w64_chunk(b"data", &PCM),
        w64_chunk(b"bext", &bext("Inside")),
    ]);
    file.extend_from_slice(&w64_chunk(b"bext", &bext("Outside")));
    check(&file, &[1, 2], &["Inside"]);

    // A RIFF root without a bound: everything up to the end is read.
    for size in [0, u32::MAX] {
        let mut file = riff(&[fmt16(), chunk(b"data", &PCM), info("Inside")]);
        file.extend_from_slice(&info("Outside"));
        set_size32(&mut file, size);
        check(&file, &[1, 2], &["Inside", "Outside"]);
    }
}

#[test]
fn root_shorter_than_source_extends_to_present_chunks() {
    // A muxer wrote the root size with the RIFF arithmetic instead of the
    // whole stream (an ffmpeg IMA ADPCM encoder wrote the root 16 bytes
    // short, with the `data` chunk ending exactly at the end of the file).
    // A chunk that is fully present in the source is accepted and extends
    // the declared end; the audio decodes.
    // RIFF.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM)]);
    assert_eq!(file.len(), 48);
    set_size32(&mut file, 38);
    assert_eq!(drain::<i16>(&mut open(file).unwrap()).unwrap()[0], [1, 2]);

    // RIFX.
    let mut file = wave(b"RIFX", &[fmt16_be(), chunk_be(b"data", &[0, 1, 0, 2])]);
    file[4..8].copy_from_slice(&38u32.to_be_bytes());
    assert_eq!(open(file).unwrap().tracks()[0].params.total_frames, Some(2));

    // RF64 and BW64: the root size in the stream header, or in `ds64`.
    for magic in [b"RF64", b"BW64"] {
        let mut file = wave(
            magic,
            &[chunk(b"ds64", &ds64(4, 0)), fmt16(), chunk(b"data", &PCM)],
        );
        assert_eq!(file.len(), 84);
        set_size32(&mut file, 74);
        assert_eq!(
            open(file.clone()).unwrap().tracks()[0].params.total_frames,
            Some(2)
        );
        set_size32(&mut file, u32::MAX);
        set_ds64_riff_size(&mut file, 74);
        assert_eq!(open(file).unwrap().tracks()[0].params.total_frames, Some(2));
    }

    // Wave64, five bytes short of the source but with the chunks fully
    // present: accepted, like the eight-byte-short ffmpeg shape.
    let mut file = w64(&[w64_chunk(b"fmt ", &fmt18()), w64_chunk(b"data", &PCM)]);
    assert_eq!(file.len(), 120);
    file[16..24].copy_from_slice(&115u64.to_le_bytes());
    assert_eq!(open(file).unwrap().tracks()[0].params.total_frames, Some(2));

    // A metadata chunk crossing the declared end, also when metadata is not
    // read (the chunk list is walked to the end for `fmt `).
    let mut file = riff(&[chunk(b"data", &PCM), info("Crossing"), fmt16()]);
    let size = file.len() as u32 - 8 - 24 - 2;
    set_size32(&mut file, size);
    assert_eq!(
        titles(&mut open_with(file.clone(), &meta(true)).unwrap()),
        vec!["Crossing"]
    );
    assert_eq!(
        drain::<i16>(&mut open_with(file, &meta(false)).unwrap()).unwrap()[0],
        [1, 2]
    );

    // Root sizes too small for the form type are still invalid.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM)]);
    set_size32(&mut file, 3);
    assert_eq!(failure(file), (ErrorKind::InvalidHeader, Some(0)));
    let mut file = wave(
        b"RF64",
        &[chunk(b"ds64", &ds64(4, 0)), fmt16(), chunk(b"data", &PCM)],
    );
    set_size32(&mut file, u32::MAX);
    set_ds64_riff_size(&mut file, 3);
    assert_eq!(failure(file), (ErrorKind::InvalidHeader, Some(0)));

    // An RF64 chunk with a 32-bit size of 0xFFFF_FFFF that `ds64` does not
    // list has no size.
    let file = wave(
        b"RF64",
        &[
            chunk(b"ds64", &ds64(4, 0)),
            fmt16(),
            chunk(b"data", &PCM),
            raw_chunk(b"junk", u32::MAX, &[0; 4]),
        ],
    );
    assert_eq!(failure(file.clone()), (ErrorKind::InvalidHeader, Some(92)));
    // Without metadata, the walk ends at `data`.
    assert!(open_with(file, &meta(false)).is_ok());

    // A truncated file: the root and a metadata chunk end past the source.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM), info("Truncated")]);
    file.truncate(file.len() - 4);
    assert_eq!(
        open_with(file.clone(), &meta(true)).unwrap_err().kind,
        ErrorKind::UnexpectedEof
    );
    let mut demuxer = open_with(file, &meta(false)).unwrap();
    assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0], [1, 2]);
}

#[test]
fn chunks_not_fully_present_are_invalid() {
    let invalid = |at| (ErrorKind::InvalidHeader, Some(at));
    // RIFF: a `data` chunk that is not fully present in the source, though
    // it starts inside a root that covers the file. Only the error offset
    // (the payload of `data`) stays as before.
    let mut file = riff(&[fmt16(), chunk(b"data", &PCM)]);
    file[40..44].copy_from_slice(&0x0000_1000u32.to_le_bytes());
    set_size32(&mut file, 38);
    assert_eq!(failure(file), invalid(44));

    // RIFX.
    let mut file = wave(b"RIFX", &[fmt16_be(), chunk_be(b"data", &[0, 1, 0, 2])]);
    file[40..44].copy_from_slice(&0x0000_1000u32.to_be_bytes());
    file[4..8].copy_from_slice(&38u32.to_be_bytes());
    assert_eq!(failure(file), invalid(44));

    // A metadata chunk whose size escapes both the root and the source,
    // before the `fmt ` chunk: the walk that goes looking for `fmt ` fails
    // there, with and without metadata.
    let file = riff(&[
        raw_chunk(b"LIST", 0x0000_1000, b"INFO"),
        fmt16(),
        chunk(b"data", &PCM),
    ]);
    assert_eq!(failure(file.clone()), invalid(20));
    assert_eq!(
        open_with(file, &meta(false)).unwrap_err().kind,
        ErrorKind::InvalidHeader
    );
}

#[test]
fn unusual_chunk_orders() {
    // `data` before `fmt `, a list between them.
    let file = riff(&[chunk(b"data", &PCM), info("Between"), fmt16()]);
    check(&file, &[1, 2], &["Between"]);
    let file = wave(
        b"RIFX",
        &[
            chunk_be(b"data", &[0, 1, 0, 2]),
            info_with("Between", chunk_be),
            fmt16_be(),
        ],
    );
    check(&file, &[1, 2], &["Between"]);
    let file = w64(&[
        w64_chunk(b"data", &PCM),
        w64_chunk(b"bext", &bext("Between")),
        w64_chunk(b"fmt ", &fmt18()),
    ]);
    check(&file, &[1, 2], &["Between"]);
    // RF64: `ds64` after a `JUNK` chunk, metadata between `fmt ` and `data`.
    let file = wave(
        b"RF64",
        &[
            chunk(b"JUNK", &[0; 6]),
            chunk(b"ds64", &ds64(4, 0)),
            fmt16(),
            info("Between"),
            raw_chunk(b"data", u32::MAX, &PCM),
        ],
    );
    check(&file, &[1, 2], &["Between"]);
}

#[test]
fn empty_data_chunk() {
    // As the last chunk.
    let file = riff(&[fmt16(), chunk(b"data", &[])]);
    let demuxer = open(file.clone()).unwrap();
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(0));
    check(&file, &[], &[]);

    // Followed by other chunks, which are not audio.
    let file = riff(&[fmt16(), chunk(b"data", &[]), info("After")]);
    check(&file, &[], &["After"]);
    // The same in a truncated file (a root beyond its end).
    let mut truncated = file.clone();
    set_size32(&mut truncated, file.len() as u32 - 8 + 100);
    check(&truncated, &[], &["After"]);
    // An invalid root is an error, not a stream.
    let mut invalid = file.clone();
    set_size32(&mut invalid, 2);
    assert_eq!(failure(invalid).0, ErrorKind::InvalidHeader);

    // RIFX, RF64 (with an empty `ds64` data size) and Wave64.
    let file = wave(
        b"RIFX",
        &[
            fmt16_be(),
            chunk_be(b"data", &[]),
            info_with("After", chunk_be),
        ],
    );
    check(&file, &[], &["After"]);
    let file = wave(
        b"RF64",
        &[
            chunk(b"ds64", &ds64(0, 0)),
            fmt16(),
            chunk(b"data", &[]),
            info("After"),
        ],
    );
    check(&file, &[], &["After"]);
    let file = w64(&[
        w64_chunk(b"fmt ", &fmt18()),
        w64_chunk(b"data", &[]),
        w64_chunk(b"bext", &bext("After")),
    ]);
    check(&file, &[], &["After"]);

    // An unfinished recording: no root size and no data size, the audio
    // runs to the end.
    for size in [0, u32::MAX] {
        let mut file = riff(&[fmt16(), raw_chunk(b"data", 0, &PCM)]);
        set_size32(&mut file, size);
        check(&file, &[1, 2], &[]);
    }
    let mut file = wave(b"RIFX", &[fmt16_be(), raw_chunk(b"data", 0, &[0, 1, 0, 2])]);
    set_size32(&mut file, 0);
    check(&file, &[1, 2], &[]);
    let mut file = wave(
        b"RF64",
        &[
            chunk(b"ds64", &ds64(0, 0)),
            fmt16(),
            raw_chunk(b"data", 0, &PCM),
        ],
    );
    set_size32(&mut file, 0);
    check(&file, &[1, 2], &[]);

    // The ambiguous case, documented: without a root size, an empty `data`
    // chunk followed by a list cannot be told from an unfinished recording,
    // and the list is read as audio.
    let mut file = riff(&[fmt16(), chunk(b"data", &[]), info("After")]);
    set_size32(&mut file, 0);
    let list = info("After");
    let samples: Vec<i16> = list
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect();
    assert_eq!(samples.len(), 13);
    check(&file, &samples, &[]);
}

fn limits(limits: FormatLimits) -> DemuxOptions {
    DemuxOptions::new().with_limits(limits)
}

fn packet_limit(bytes: usize) -> DemuxOptions {
    limits(FormatLimits::new().with_max_packet_bytes(bytes))
}

fn packet_sizes(demuxer: &mut dyn Demuxer) -> Vec<usize> {
    let mut packet = Packet::default();
    let mut sizes = Vec::new();
    while demuxer.next_packet(&mut packet).unwrap() {
        sizes.push(packet.data.len());
    }
    sizes
}

#[test]
fn packet_and_track_limits() {
    let limit_exceeded = |file: &[u8], options: &DemuxOptions| {
        open_with(file.to_vec(), options).unwrap_err().kind == ErrorKind::LimitExceeded
    };

    // PCM: a packet holds whole frames of 2 bytes.
    let pcm = riff(&[fmt16(), chunk(b"data", &[1, 0, 2, 0, 3, 0])]);
    assert!(limit_exceeded(&pcm, &packet_limit(0)));
    assert!(limit_exceeded(&pcm, &packet_limit(1)));
    for (bytes, sizes) in [
        (2, &[2, 2, 2][..]),
        (3, &[2, 2, 2]),
        (4, &[4, 2]),
        (usize::MAX, &[6]),
    ] {
        let mut demuxer = open_with(pcm.clone(), &packet_limit(bytes)).unwrap();
        assert_eq!(packet_sizes(&mut demuxer), sizes, "limit {bytes}");
    }

    // IMA ADPCM: whole blocks of 36 bytes.
    let mut ima_fmt = fmt(0x11, 1, 8000, 36, 4);
    ima_fmt.extend_from_slice(&[2, 0, 65, 0]);
    let ima = riff(&[chunk(b"fmt ", &ima_fmt), chunk(b"data", &[0; 72])]);
    assert!(limit_exceeded(&ima, &packet_limit(0)));
    assert!(limit_exceeded(&ima, &packet_limit(35)));
    for (bytes, sizes) in [(36, &[36, 36][..]), (71, &[36, 36]), (72, &[72])] {
        let mut demuxer = open_with(ima.clone(), &packet_limit(bytes)).unwrap();
        assert_eq!(packet_sizes(&mut demuxer), sizes, "limit {bytes}");
    }

    // A compressed stream: any number of bytes but 0.
    let mp3 = riff(&[
        chunk(b"fmt ", &fmt(0x55, 1, 8000, 1, 0)),
        chunk(b"data", &[0xAB; 10]),
    ]);
    assert!(limit_exceeded(&mp3, &packet_limit(0)));
    let mut demuxer = open_with(mp3, &packet_limit(3)).unwrap();
    assert_eq!(packet_sizes(&mut demuxer), [3, 3, 3, 1]);

    // A WAVE file has one track.
    assert!(limit_exceeded(
        &pcm,
        &limits(FormatLimits::new().with_max_tracks(0))
    ));
    let demuxer = open_with(pcm, &limits(FormatLimits::new().with_max_tracks(1))).unwrap();
    assert_eq!(demuxer.tracks().len(), 1);
}

/// A `cue ` chunk with `(identifier, frame)` points in `data`.
fn cue(points: impl ExactSizeIterator<Item = (u32, u32)>) -> Vec<u8> {
    let mut out = (points.len() as u32).to_le_bytes().to_vec();
    for (id, frame) in points {
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&frame.to_le_bytes());
    }
    chunk(b"cue ", &out)
}

/// A `labl` or `note` entry.
fn label(kind: &[u8; 4], id: u32, text: &str) -> Vec<u8> {
    let mut entry = id.to_le_bytes().to_vec();
    entry.extend_from_slice(text.as_bytes());
    entry.push(0);
    chunk(kind, &entry)
}

/// An `ltxt` entry with a length in frames.
fn region(id: u32, length: u32) -> Vec<u8> {
    let mut entry = id.to_le_bytes().to_vec();
    entry.extend_from_slice(&length.to_le_bytes());
    entry.extend_from_slice(&[0; 12]);
    chunk(b"ltxt", &entry)
}

fn adtl(entries: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut list = b"adtl".to_vec();
    for entry in entries {
        list.extend_from_slice(&entry);
    }
    chunk(b"LIST", &list)
}

/// One second of silence with `chunks` after it.
fn with_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut all = vec![fmt16(), chunk(b"data", &vec![0; 16_000])];
    all.extend_from_slice(chunks);
    riff(&all)
}

/// An ID3v2.3 tag of the given frames as an `id3 ` chunk.
fn id3_chunk(frames: &[u8]) -> Vec<u8> {
    let size = frames.len() as u32;
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    for shift in [21, 14, 7, 0] {
        tag.push((size >> shift) as u8 & 0x7F);
    }
    tag.extend_from_slice(frames);
    chunk(b"id3 ", &tag)
}

/// An ID3v2.3 frame.
fn id3_frame(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(payload);
    out
}

/// `count` `CHAP` frames.
fn id3_chapters(count: usize) -> Vec<u8> {
    let mut payload = b"c\0".to_vec();
    payload.extend_from_slice(&[0; 8]);
    payload.extend_from_slice(&[0xFF; 8]);
    id3_frame(b"CHAP", &payload).repeat(count)
}

#[test]
fn duplicate_cue_points_and_labels() {
    // 128 points with one identifier, each with a title, a note and a
    // region: the first point and the first entry of each kind count.
    let points = (0..128).map(|i| (1, 1000 + i));
    let entries = (0..128u32).flat_map(|i| {
        [
            label(b"labl", 1, &format!("Title {i}")),
            label(b"note", 1, &format!("Note {i}")),
            region(1, 100 + i),
        ]
    });
    let file = with_chunks(&[cue(points), adtl(entries)]);
    let demuxer = open_with(file, &meta(true)).unwrap();
    let chapters = demuxer.chapters();
    assert_eq!(chapters.len(), 1);
    let chapter = &chapters[0];
    assert_eq!(chapter.id, "1");
    // Frames 1000 and 1100 at 8 kHz.
    assert_eq!(chapter.start, Duration::from_millis(125));
    assert_eq!(chapter.end, Some(Duration::from_micros(137_500)));
    let tags: Vec<_> = chapter
        .tags
        .iter()
        .map(|tag| (tag.raw.key.as_str(), tag.raw.value.clone()))
        .collect();
    assert_eq!(
        tags,
        [
            ("LABL", RawValue::Text("Title 0".into())),
            ("NOTE", RawValue::Text("Note 0".into())),
        ]
    );

    // 100 000 points with one identifier: one chapter.
    let file = with_chunks(&[cue((0..100_000).map(|i| (7, i % 16_000)))]);
    assert_eq!(open_with(file, &meta(true)).unwrap().chapters().len(), 1);

    // Labels without a point are not chapters.
    let file = with_chunks(&[cue([(1, 0)].into_iter()), adtl([label(b"labl", 2, "None")])]);
    let demuxer = open_with(file, &meta(true)).unwrap();
    assert_eq!(demuxer.chapters().len(), 1);
    assert!(demuxer.chapters()[0].tags.is_empty());
}

#[test]
fn chapter_and_field_budgets() {
    let limit_exceeded =
        |file: Vec<u8>| open_with(file, &meta(true)).unwrap_err().kind == ErrorKind::LimitExceeded;

    // Distinct cue points: 65 536 chapters at most.
    let file = with_chunks(&[cue((0..65_536).map(|i| (i, i % 16_000)))]);
    assert_eq!(
        open_with(file, &meta(true)).unwrap().chapters().len(),
        65_536
    );
    assert!(limit_exceeded(with_chunks(&[cue(
        (0..65_537).map(|i| (i, 0))
    )])));
    // Labels of distinct identifiers: 65 536 at most.
    let labels = (0..65_537).map(|i| label(b"labl", i, ""));
    assert!(limit_exceeded(with_chunks(&[adtl(labels)])));

    // One budget for the chapters of all `id3 ` chunks and cue points, in
    // either order.
    let chapters = id3_chunk(&id3_chapters(40_000));
    let points = || cue((0..30_000).map(|i| (i, 0)));
    let file = with_chunks(std::slice::from_ref(&chapters));
    assert_eq!(
        open_with(file, &meta(true)).unwrap().chapters().len(),
        40_000
    );
    assert!(limit_exceeded(with_chunks(&[chapters.clone(), points()])));
    assert!(limit_exceeded(with_chunks(&[points(), chapters.clone()])));
    assert!(limit_exceeded(with_chunks(&[chapters.clone(), chapters])));

    // One budget for the fields of all `id3 ` chunks.
    let fields = id3_chunk(&id3_frame(b"TIT2", b"\0x").repeat(40_000));
    let mut demuxer = open_with(with_chunks(std::slice::from_ref(&fields)), &meta(true)).unwrap();
    assert_eq!(texts(&mut demuxer, "TIT2").len(), 40_000);
    assert!(limit_exceeded(with_chunks(&[fields.clone(), fields])));
}

/// Result of opening `file` with `options`: the first letter of each text
/// field named `key`, or the kind and offset of the error.
fn initials(
    file: &[u8],
    options: &DemuxOptions,
    key: &str,
) -> Result<String, (ErrorKind, Option<u64>)> {
    match open_with(file.to_vec(), options) {
        Ok(mut demuxer) => Ok(texts(&mut demuxer, key)
            .iter()
            .map(|text| text.chars().next().unwrap_or('-'))
            .collect()),
        Err(err) => Err((err.kind, err.byte_offset)),
    }
}

fn over_budget(at: u64) -> Result<String, (ErrorKind, Option<u64>)> {
    Err((ErrorKind::LimitExceeded, Some(at)))
}

fn byte_budget(bytes: usize) -> DemuxOptions {
    limits(FormatLimits::new().with_max_metadata_bytes(bytes))
}

#[test]
fn metadata_byte_budget_is_shared() {
    // Two lists with a payload of 4 + 8 + 500 bytes each, after the 16 044
    // bytes of the header, `fmt ` and `data`.
    let long = |c: char| String::from(c).repeat(499);
    let file = with_chunks(&[info(&long('a')), info(&long('b'))]);
    let read = |file: &[u8], bytes| initials(file, &byte_budget(bytes), "INAM");
    assert_eq!(read(&file, 1024), Ok("ab".into()));
    // A list that does not fit is an error, whether or not one did.
    assert_eq!(read(&file, 1023), over_budget(16_052 + 520));
    assert_eq!(read(&file, 512), over_budget(16_052 + 520));
    assert_eq!(read(&file, 511), over_budget(16_052));
    assert_eq!(read(&file, 0), over_budget(16_052));
    // Unknown chunks are not read and cost nothing.
    let junk = chunk(b"JUNK", &[0; 1000]);
    let padded = with_chunks(&[junk.clone(), info(&long('a')), junk, info(&long('b'))]);
    assert_eq!(read(&padded, 1024), Ok("ab".into()));
    // Without metadata nothing is read or taken.
    let quiet = byte_budget(0).with_read_metadata(false);
    assert_eq!(initials(&file, &quiet, "INAM"), Ok(String::new()));

    // A leading ID3v2 tag of 10 + 10 + 591 bytes counts too.
    let mut title = vec![0];
    title.extend_from_slice("t".repeat(590).as_bytes());
    let tag = id3_chunk(&id3_frame(b"TIT2", &title));
    // The payload of the chunk, without its pad byte.
    let len = u32::from_le_bytes([tag[4], tag[5], tag[6], tag[7]]) as usize;
    let mut file_with_tag = tag[8..8 + len].to_vec();
    assert_eq!(file_with_tag.len(), 611);
    file_with_tag.extend_from_slice(&with_chunks(&[info(&long('a'))]));
    let both = |bytes| {
        let options = byte_budget(bytes);
        Ok::<_, (ErrorKind, Option<u64>)>((
            initials(&file_with_tag, &options, "TIT2")?,
            initials(&file_with_tag, &options, "INAM")?,
        ))
    };
    assert_eq!(both(1123), Ok(("t".into(), "a".into())));
    assert_eq!(
        both(1122),
        Err((ErrorKind::LimitExceeded, Some(611 + 16_052)))
    );
    assert_eq!(both(610), Err((ErrorKind::LimitExceeded, Some(0))));
}

#[test]
fn metadata_entry_budget_is_shared() {
    // 65 536 entries for the fields of all chunks together.
    let fields = |count: usize| id3_chunk(&id3_frame(b"TIT2", b"\0x").repeat(count));
    let file = with_chunks(&[fields(65_535), info("a")]);
    let mut demuxer = open_with(file, &meta(true)).unwrap();
    assert_eq!(
        texts(&mut demuxer, "TIT2").len() + texts(&mut demuxer, "INAM").len(),
        65_536
    );
    let file = with_chunks(&[info("a"), fields(65_535), info("b")]);
    let err = open_with(file, &meta(true)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::LimitExceeded);
    // Labels, cue points and fields share them as well.
    let file = with_chunks(&[cue((0..65_535).map(|i| (i, 0))), info("a")]);
    assert!(open_with(file, &meta(true)).is_ok());
    let file = with_chunks(&[
        cue((0..65_535).map(|i| (i, 0))),
        adtl([label(b"labl", 0, "x")]),
        info("a"),
    ]);
    let err = open_with(file, &meta(true)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::LimitExceeded);
}

#[test]
fn revisions_follow_the_chunks() {
    let title = |text: &str| {
        let mut payload = vec![0];
        payload.extend_from_slice(text.as_bytes());
        id3_chunk(&id3_frame(b"TIT2", &payload))
    };
    let keys_and_values = |file: Vec<u8>, read: bool| {
        let mut demuxer = open_with(file, &meta(read)).unwrap();
        demuxer
            .metadata()
            .iter()
            .map(|revision| {
                revision
                    .tags
                    .iter()
                    .map(|tag| match &tag.raw.value {
                        RawValue::Text(text) => format!("{}={text}", tag.raw.key),
                        other => format!("{}={other:?}", tag.raw.key),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
    };
    // Before `data`: the `id3 ` chunk, then the list.
    let file = riff(&[fmt16(), title("one"), info("two"), chunk(b"data", &PCM)]);
    assert_eq!(keys_and_values(file, true), ["TIT2=one", "INAM=two"]);
    // Alternating chunks with conflicting values, after `data`: each run of
    // one kind is a revision, nothing is merged across.
    let file = with_chunks(&[info("a"), title("b"), info("c"), title("d")]);
    assert_eq!(
        keys_and_values(file.clone(), true),
        ["INAM=a", "TIT2=b", "INAM=c", "TIT2=d"]
    );
    assert!(keys_and_values(file, false).is_empty());
    // Consecutive chunks of one kind share a revision.
    let file = with_chunks(&[info("a"), info("b"), title("c"), title("d")]);
    assert_eq!(
        keys_and_values(file, true),
        ["INAM=a,INAM=b", "TIT2=c,TIT2=d"]
    );
    // A leading ID3v2 tag comes first.
    let tag = title("lead");
    let len = u32::from_le_bytes([tag[4], tag[5], tag[6], tag[7]]) as usize;
    let mut file = tag[8..8 + len].to_vec();
    file.extend_from_slice(&riff(&[
        fmt16(),
        info("x"),
        title("y"),
        chunk(b"data", &PCM),
    ]));
    assert_eq!(
        keys_and_values(file, true),
        ["TIT2=lead", "INAM=x", "TIT2=y"]
    );
}

/// A Wave64 chunk whose size field declares `declared` bytes of payload,
/// whatever the payload really is.
fn w64_chunk_declaring(id: &[u8; 4], declared: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = crate::containers::w64_guid(id).to_vec();
    out.extend_from_slice(&declared.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Sets the 64-bit root size of a Wave64 file.
fn set_w64_root(file: &mut [u8], size: u64) {
    assert_eq!(&file[..4], b"riff");
    file[16..24].copy_from_slice(&size.to_le_bytes());
}

#[test]
fn wave64_root_written_with_riff_arithmetic() {
    // ffmpeg's Wave64 muxer wrote the root size with the RIFF arithmetic,
    // `file size - 8`, instead of the Wave64 one (the whole file), so a `data`
    // chunk that fills the file ends eight bytes past the declared root.
    // `w64/w64-pcm16.w64` in the FFmpeg FATE suite is such a file. The
    // fixture is ten bytes of audio (five frames) so that a root nine bytes
    // short still holds the whole 24-byte header of `data`: the chunk is then
    // found and rejected at its payload, not lost.
    const PCM5: [u8; 10] = [1, 0, 2, 0, 3, 0, 4, 0, 5, 0];
    let mut file = w64(&[w64_chunk(b"fmt ", &fmt18()), w64_chunk(b"data", &PCM5)]);
    assert_eq!(file.len(), 128);
    set_w64_root(&mut file, 128 - 8);
    let demuxer = open(file.clone()).unwrap();
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(5));
    assert_eq!(
        crate::common::decode::<i16>(file.clone()).unwrap()[0],
        [1, 2, 3, 4, 5]
    );
    // The same file through `Probe`: the audio range is not cut by the root.
    let mut probe = akousma_core::probe::Probe::new();
    akousma_wav::register_demuxer(&mut probe);
    probe.set_anchor_scanner(Box::new(akousma_meta::anchors::TagScanner));
    let probed = probe
        .open(
            &akousma_core::probe::Hint::new(),
            std::io::Cursor::new(file),
            &DemuxOptions::new(),
        )
        .unwrap();
    let probed_demuxer = probed.demuxer;
    assert_eq!(probed_demuxer.tracks()[0].params.total_frames, Some(5));

    // One byte more or less is not that shape either, but the `data` chunk
    // is fully present in the source, so it decodes like every other
    // tolerated root: what is present is read, what is not is an error.
    for size in [128 - 9, 128 - 7] {
        let mut file = w64(&[w64_chunk(b"fmt ", &fmt18()), w64_chunk(b"data", &PCM5)]);
        set_w64_root(&mut file, size);
        assert_eq!(open(file).unwrap().tracks()[0].params.total_frames, Some(5));
    }

    // A root so small that the header of `data` does not fit ends the chunk
    // list before it: this is "there is no `data`", not "the root is short by
    // nine bytes", so the error comes from the check that found no audio.
    let mut file = w64(&[w64_chunk(b"fmt ", &fmt18()), w64_chunk(b"data", &PCM5)]);
    set_w64_root(&mut file, 111);
    assert_eq!(failure(file), (ErrorKind::InvalidHeader, Some(0)));

    // The recognised root does not let `data` run past the source: the chunk
    // is still checked against the root, which is now the end of the file, so
    // a `data` that declares more than the source holds is `InvalidHeader`
    // here as in every other container.
    let mut file = w64(&[
        w64_chunk(b"fmt ", &fmt18()),
        w64_chunk_declaring(b"data", 24 + 18, &PCM5),
    ]);
    let len = file.len() as u64;
    set_w64_root(&mut file, len - 8);
    assert_eq!(failure(file), (ErrorKind::InvalidHeader, Some(112)));
}
