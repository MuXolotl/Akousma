//! The five containers, streamed and truncated files, odd offsets and
//! hostile sizes.

use std::io::Cursor;

use akousma_core::probe::{Demuxer, Hint, Probe};
use akousma_core::{CodecId, DemuxOptions, ErrorKind, FormatLimits};
use akousma_meta::anchors::TagScanner;
use akousma_wav::WavContainer;

use crate::common::{chunk, chunk_be, decode, drain, fmt, open, open_with, pcm16, riff, wave};

const W64_RIFF: [u8; 16] = [
    0x72, 0x69, 0x66, 0x66, 0x2E, 0x91, 0xCF, 0x11, 0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00, 0x00,
];
const W64_SUFFIX: [u8; 12] = [
    0xF3, 0xAC, 0xD3, 0x11, 0x8C, 0xD1, 0x00, 0xC0, 0x4F, 0x8E, 0xDB, 0x8A,
];

pub fn w64_guid(name: &[u8; 4]) -> [u8; 16] {
    let mut guid = [0; 16];
    guid[..4].copy_from_slice(name);
    guid[4..].copy_from_slice(&W64_SUFFIX);
    guid
}

pub fn w64_chunk(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = w64_guid(name).to_vec();
    out.extend_from_slice(&(payload.len() as u64 + 24).to_le_bytes());
    out.extend_from_slice(payload);
    out.resize(out.len() + (payload.len().wrapping_neg() & 7), 0);
    out
}

pub fn w64(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body = chunks.concat();
    let mut out = W64_RIFF.to_vec();
    out.extend_from_slice(&(body.len() as u64 + 40).to_le_bytes());
    out.extend_from_slice(&w64_guid(b"wave"));
    out.extend_from_slice(&body);
    out
}

pub fn ds64(data_size: u64, sample_count: u64) -> Vec<u8> {
    let mut out = 0u64.to_le_bytes().to_vec();
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&sample_count.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// A chunk with an explicit 32-bit size field.
pub fn raw_chunk(id: &[u8; 4], size: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

const SAMPLES: [i16; 6] = [i16::MIN, i16::MAX, 1, -1, 300, -300];

fn stereo(planes: &[Vec<i16>]) {
    assert_eq!(planes[0], [i16::MIN, 1, 300]);
    assert_eq!(planes[1], [i16::MAX, -1, -300]);
}

fn data16() -> Vec<u8> {
    SAMPLES.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[test]
fn containers_riff_rifx_rf64_bw64_w64() {
    // RIFF.
    let demuxer = open(pcm16(2, &SAMPLES)).unwrap();
    assert_eq!(demuxer.container(), WavContainer::Riff);
    stereo(&decode::<i16>(pcm16(2, &SAMPLES)).unwrap());

    // RIFX: big-endian fields and samples.
    let mut fmt_be = Vec::new();
    for field in [1u16, 2] {
        fmt_be.extend_from_slice(&field.to_be_bytes());
    }
    fmt_be.extend_from_slice(&8000u32.to_be_bytes());
    fmt_be.extend_from_slice(&32000u32.to_be_bytes());
    fmt_be.extend_from_slice(&4u16.to_be_bytes());
    fmt_be.extend_from_slice(&16u16.to_be_bytes());
    let data_be: Vec<u8> = SAMPLES.iter().flat_map(|s| s.to_be_bytes()).collect();
    let rifx = wave(
        b"RIFX",
        &[chunk_be(b"fmt ", &fmt_be), chunk_be(b"data", &data_be)],
    );
    let mut demuxer = open(rifx).unwrap();
    assert_eq!(demuxer.container(), WavContainer::Rifx);
    assert_eq!(demuxer.tracks()[0].params.codec, CodecId::PcmS16Be);
    stereo(&drain::<i16>(&mut demuxer).unwrap());

    // RF64: a data size of 0xFFFF_FFFF takes the 64-bit size of `ds64`;
    // the 8 bytes after the 12 it declares are outside the container.
    let mut rf64 = wave(
        b"RF64",
        &[
            chunk(b"JUNK", &[0; 4]),
            chunk(b"ds64", &ds64(12, 0)),
            chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)),
            raw_chunk(b"data", u32::MAX, &data16()),
        ],
    );
    rf64.extend_from_slice(&[0xAA; 8]);
    let mut demuxer = open(rf64.clone()).unwrap();
    assert_eq!(demuxer.container(), WavContainer::Rf64);
    stereo(&drain::<i16>(&mut demuxer).unwrap());
    // Inside the container, they would be a chunk that crosses its end.
    // JUNK 12, ds64 36, fmt 24, data 8 + 12, then the 8 bytes.
    set_riff_size(&mut rf64, 4 + 12 + 36 + 24 + 20 + 8);
    let err = open(rf64).unwrap_err();
    assert_eq!(
        (err.kind, err.byte_offset),
        (ErrorKind::InvalidHeader, Some(112))
    );

    // RF64 under 4 GiB: a real 32-bit size wins over `ds64`.
    let rf64 = wave(
        b"RF64",
        &[
            chunk(b"ds64", &ds64(1 << 40, 0)),
            chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)),
            chunk(b"data", &data16()),
            chunk(b"LIST", b"INFO"),
        ],
    );
    stereo(&decode::<i16>(rf64).unwrap());

    // RF64 and BW64 need `ds64` before `fmt ` and `data`.
    for magic in [b"RF64", b"BW64"] {
        let late = wave(
            magic,
            &[
                chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)),
                chunk(b"ds64", &ds64(12, 0)),
            ],
        );
        assert_eq!(open(late).unwrap_err().kind, ErrorKind::InvalidHeader);
    }
    let short = wave(b"RF64", &[chunk(b"ds64", &[0; 20])]);
    assert_eq!(open(short).unwrap_err().kind, ErrorKind::InvalidHeader);

    // BW64: the third `ds64` field is unused, also for ADPCM.
    let mut ima_fmt = fmt(0x11, 1, 8000, 36, 4);
    ima_fmt.extend_from_slice(&[2, 0, 65, 0]);
    let bw64 = wave(
        b"BW64",
        &[
            chunk(b"ds64", &ds64(36, 1)),
            chunk(b"fmt ", &ima_fmt),
            raw_chunk(b"data", u32::MAX, &[0; 36]),
        ],
    );
    let demuxer = open(bw64).unwrap();
    assert_eq!(demuxer.container(), WavContainer::Bw64);
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(65));

    // Sony Wave64 with an 18-byte `fmt `: 6 pad bytes before `data`.
    let mut fmt18 = fmt(1, 2, 8000, 4, 16);
    fmt18.extend_from_slice(&[0, 0]);
    let file = w64(&[w64_chunk(b"fmt ", &fmt18), w64_chunk(b"data", &data16())]);
    assert_eq!(file.len(), 40 + 24 + 24 + 24 + 12 + 4);
    let mut demuxer = open(file).unwrap();
    assert_eq!(demuxer.container(), WavContainer::Wave64);
    stereo(&drain::<i16>(&mut demuxer).unwrap());

    // Probe: WAVE forms only.
    let mut probe = Probe::new();
    akousma_wav::register_demuxer(&mut probe);
    let hint = Hint::new();
    for magic in [b"RIFF", b"RIFX", b"RF64", b"BW64"] {
        assert!(probe.detect(&hint, &wave(magic, &[])).is_some());
    }
    assert!(probe.detect(&hint, &w64(&[])).is_some());
    let mut webp = riff(&[]);
    webp[8..12].copy_from_slice(b"WEBP");
    let mut avi = riff(&[]);
    avi[8..12].copy_from_slice(b"AVI ");
    assert!(probe.detect(&hint, &webp).is_none());
    assert!(probe.detect(&hint, &avi).is_none());
    assert!(probe.detect(&hint, b"RIFF").is_none());
}

#[test]
fn streaming_and_unaligned_data_tail() {
    // Streamed RIFF: both sizes 0xFFFF_FFFF, audio up to the end.
    let mut file = b"RIFF\xFF\xFF\xFF\xFFWAVE".to_vec();
    file.extend_from_slice(&chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)));
    file.extend_from_slice(&raw_chunk(b"data", u32::MAX, &data16()));
    let demuxer = open(file.clone()).unwrap();
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(3));
    stereo(&decode::<i16>(file).unwrap());

    // Unfinished recording: sizes 0.
    let mut file = b"RIFF\x00\x00\x00\x00WAVE".to_vec();
    file.extend_from_slice(&chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)));
    file.extend_from_slice(&raw_chunk(b"data", 0, &data16()));
    stereo(&decode::<i16>(file).unwrap());

    // 1 and 3 stray bytes after the last whole frame are dropped.
    for extra in [1, 3] {
        let mut data = data16();
        data.resize(data.len() + extra, 0x55);
        let file = riff(&[
            chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)),
            chunk(b"data", &data),
        ]);
        let mut demuxer = open(file).unwrap();
        assert_eq!(demuxer.tracks()[0].params.total_frames, Some(3));
        let seek = demuxer
            .seek(akousma_core::SeekMode::Accurate, frame(2))
            .unwrap();
        assert_eq!(seek.actual_ts, 2);
        let planes = drain::<i16>(&mut demuxer).unwrap();
        assert_eq!(
            (planes[0].as_slice(), planes[1].as_slice()),
            (&[300][..], &[-300][..])
        );
    }

    // An odd last chunk without its pad byte ends the file normally.
    let mut file = riff(&[
        chunk(b"fmt ", &fmt(1, 1, 8000, 1, 8)),
        chunk(b"data", &[0, 128, 255]),
    ]);
    assert_eq!(file.pop(), Some(0));
    assert_eq!(
        decode::<i16>(file.clone()).unwrap()[0],
        [i16::MIN, 0, 32512]
    );
    let mut with_list = riff(&[
        chunk(b"fmt ", &fmt(1, 1, 8000, 1, 8)),
        chunk(b"data", &[0, 0]),
        chunk(b"LIST", b"INFOINAM\x03\x00\x00\x00abc"),
    ]);
    assert_eq!(with_list.pop(), Some(0));
    let mut demuxer = open(with_list).unwrap();
    assert_eq!(demuxer.metadata().len(), 1);

    // A truncated file, with root and data sizes far beyond its end: the
    // bytes present.
    let mut file = riff(&[
        chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)),
        raw_chunk(b"data", 16 << 20, &data16()),
    ]);
    assert!(file.len() < 64);
    set_riff_size(&mut file, 4 + 24 + 8 + (16 << 20));
    assert_eq!(open(file).unwrap().tracks()[0].params.total_frames, Some(3));
}

/// A file whose `data` declares `0xFFFF_FFFF` cannot tell audio from a
/// trailing `LIST`: the header declares no boundary, so everything after the
/// audio is decoded as audio. No chunk of a streamed `data` can be recognised
/// without a guess, so this behaviour is pinned here and documented as a
/// limitation of the format rather than treated as a bug.
#[test]
fn streamed_data_swallows_trailing_list() {
    let mut base = b"RIFF\xFF\xFF\xFF\xFFWAVE".to_vec();
    base.extend_from_slice(&chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)));
    base.extend_from_slice(&raw_chunk(b"data", u32::MAX, &data16()));
    let planes_a = decode::<i16>(base.clone()).unwrap();

    let mut with_list = base;
    with_list.extend_from_slice(&chunk(b"LIST", b"INFOINAM\x03\x00\x00\x00abc"));
    let mut demuxer = open(with_list.clone()).unwrap();
    // 12 bytes of audio + the 24-byte `LIST` chunk: 36 bytes, 9 stereo frames.
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(9));
    // The `LIST` is never even seen as a chunk: no metadata is read.
    assert_eq!(demuxer.metadata().len(), 0);
    let planes_b = decode::<i16>(with_list).unwrap();
    assert_eq!(&planes_b[0][..3], &planes_a[0][..]);
    assert_eq!(&planes_b[1][..3], &planes_a[1][..]);
}

/// Overwrites the root size of a RIFF, RF64 or BW64 file.
fn set_riff_size(file: &mut [u8], size: u32) {
    file[4..8].copy_from_slice(&size.to_le_bytes());
}

/// Overwrites the root size of a Wave64 file with its length.
fn fill_w64_size(file: &mut [u8]) {
    let len = file.len() as u64;
    file[16..24].copy_from_slice(&len.to_le_bytes());
}

fn frame(frame: u64) -> akousma_core::SeekTarget {
    akousma_core::SeekTarget::Frame { track_id: 0, frame }
}

/// An ID3v2.3 tag with one `TIT2` frame; `pad` extra zero bytes make its
/// length odd.
fn id3v2(title: &str, pad: usize) -> Vec<u8> {
    let mut frame = b"TIT2".to_vec();
    frame.extend_from_slice(&(title.len() as u32 + 1).to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0]);
    frame.extend_from_slice(title.as_bytes());
    frame.resize(frame.len() + pad, 0);
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    let size = frame.len() as u32;
    tag.extend_from_slice(&[
        (size >> 21) as u8 & 0x7F,
        (size >> 14) as u8 & 0x7F,
        (size >> 7) as u8 & 0x7F,
        size as u8 & 0x7F,
    ]);
    tag.extend_from_slice(&frame);
    tag
}

#[test]
fn riff_at_odd_offset_after_id3v2() {
    let tag = id3v2("Odd", 3);
    assert_eq!(tag.len() % 2, 1);
    let body = riff(&[
        chunk(b"junk", &[1, 2, 3]),
        chunk(b"fmt ", &fmt(1, 2, 8000, 4, 16)),
        // A repeated `fmt ` is ignored.
        chunk(b"fmt ", &fmt(1, 1, 8000, 2, 16)),
        chunk(b"data", &data16()),
    ]);
    let mut file = tag.clone();
    file.extend_from_slice(&body);

    // Direct open: the tag is skipped and read.
    let mut demuxer =
        open_with(file.clone(), &DemuxOptions::new().with_read_metadata(true)).unwrap();
    assert_eq!(demuxer.tracks()[0].params.channels, 2);
    let title = &demuxer.metadata().latest().unwrap().tags[0];
    assert_eq!(title.raw.key, "TIT2");
    stereo(&drain::<i16>(&mut demuxer).unwrap());

    // Through the probe, which removes the tag first.
    let mut probe = Probe::new();
    akousma_wav::register_demuxer(&mut probe);
    probe.set_anchor_scanner(Box::new(TagScanner));
    let mut probed = probe
        .open(&Hint::new(), Cursor::new(file), &DemuxOptions::new())
        .unwrap();
    assert_eq!(probed.format, "wav");
    assert_eq!(probed.demuxer.metadata().len(), 1);
    stereo(&drain::<i16>(&mut *probed.demuxer).unwrap());
}

#[test]
fn fuzz_cve_regressions() {
    let header = |file: Vec<u8>| open(file).unwrap_err().kind;
    // Empty, one byte, truncated headers.
    assert_eq!(header(Vec::new()), ErrorKind::UnexpectedEof);
    assert_eq!(header(b"R".to_vec()), ErrorKind::UnexpectedEof);
    assert_eq!(
        header(b"RIFF\x00\x00\x00\x00".to_vec()),
        ErrorKind::UnexpectedEof
    );
    assert_eq!(
        header(b"RIFF\x04\x00\x00\x00AVI ".to_vec()),
        ErrorKind::InvalidHeader
    );
    assert_eq!(header(riff(&[])), ErrorKind::InvalidHeader);
    assert_eq!(
        header(riff(&[chunk(b"fmt ", &fmt(1, 1, 8000, 2, 16))])),
        ErrorKind::InvalidHeader
    );
    assert_eq!(
        header(riff(&[chunk(b"data", &[0; 4])])),
        ErrorKind::InvalidHeader
    );
    assert_eq!(
        header(riff(&[chunk(b"fmt ", &[1, 0, 1, 0])])),
        ErrorKind::InvalidHeader
    );

    // Zero divisors in `fmt ` (CVE-2017-16942).
    for (channels, rate, align, bits) in [
        (0, 8000, 2, 16),
        (1, 0, 2, 16),
        (1, 8000, 0, 16),
        (1, 8000, 2, 0),
    ] {
        let file = riff(&[
            chunk(b"fmt ", &fmt(1, channels, rate, align, bits)),
            chunk(b"data", &[0; 4]),
        ]);
        assert_eq!(header(file), ErrorKind::InvalidHeader);
    }

    // Samples per block beyond the block (CVE-2026-37555).
    let mut ima = fmt(0x11, 1, 8000, 25_000, 4);
    ima.extend_from_slice(&[2, 0]);
    ima.extend_from_slice(&50_000u16.to_le_bytes());
    assert_eq!(
        header(riff(&[chunk(b"fmt ", &ima), chunk(b"data", &[0; 8])])),
        ErrorKind::InvalidHeader
    );
    // The largest legal block with a size claim of 50 000 blocks in a
    // streamed file: counted from the bytes present, without overflow.
    let mut ima = fmt(0x11, 1, 8000, 65_535, 4);
    ima.extend_from_slice(&[2, 0]);
    ima.extend_from_slice(&50_000u16.to_le_bytes());
    let mut file = riff(&[
        chunk(b"fmt ", &ima),
        raw_chunk(b"data", u32::MAX - 1, &[0; 4]),
    ]);
    set_riff_size(&mut file, u32::MAX);
    assert_eq!(
        open(file.clone()).unwrap().tracks()[0].params.total_frames,
        Some(1)
    );
    // With the root size of the bytes present, the claim crosses its end.
    set_riff_size(&mut file, 4 + 28 + 8 + 4);
    assert_eq!(header(file), ErrorKind::InvalidHeader);

    // A 2 GiB metadata chunk in a 60-byte file.
    // The root size covers it: the file is truncated.
    let mut bomb = riff(&[
        chunk(b"fmt ", &fmt(1, 1, 8000, 2, 16)),
        chunk(b"data", &[0; 2]),
        raw_chunk(b"LIST", 0x7FFF_FFFF, b"INFO"),
    ]);
    assert!(bomb.len() <= 60);
    set_riff_size(&mut bomb, 4 + 24 + 10 + 8 + 0x7FFF_FFFF);
    let meta = DemuxOptions::new().with_read_metadata(true);
    // Over the default limit: refused before anything is allocated.
    let err = open_with(bomb.clone(), &meta).unwrap_err();
    assert_eq!(
        (err.kind, err.byte_offset),
        (ErrorKind::LimitExceeded, Some(54))
    );
    // Without metadata the chunk is not read.
    let quiet = DemuxOptions::new().with_read_metadata(false);
    assert!(open_with(bomb.clone(), &quiet).is_ok());
    // Within a raised limit: read in steps, ending at the end of the file.
    let unlimited = meta.with_limits(FormatLimits::new().with_max_metadata_bytes(usize::MAX));
    assert_eq!(
        open_with(bomb, &unlimited).unwrap_err().kind,
        ErrorKind::UnexpectedEof
    );

    // A Wave64 chunk of 4 GiB + 64 bytes: compared as `u64` (cut to 32 bits
    // it would fit), over the limit.
    let mut fmt18 = fmt(1, 1, 8000, 2, 16);
    fmt18.extend_from_slice(&[0, 0]);
    let mut file = w64(&[w64_chunk(b"fmt ", &fmt18), w64_chunk(b"data", &[0; 2])]);
    file.extend_from_slice(&w64_guid(b"bext"));
    file.extend_from_slice(&(0x1_0000_0040u64 + 24).to_le_bytes());
    file.extend_from_slice(&[0; 64]);
    // A truncated file: the root size covers the chunk.
    let len = file.len() as u64 - 64 + 0x1_0000_0040;
    file[16..24].copy_from_slice(&len.to_le_bytes());
    let payload = file.len() as u64 - 64;
    let err = open_with(file, &meta).unwrap_err();
    assert_eq!(
        (err.kind, err.byte_offset),
        (ErrorKind::LimitExceeded, Some(payload))
    );

    // Wave64 chunk sizes below the header or near `u64::MAX`.
    for size in [23, u64::MAX] {
        let mut file = w64(&[w64_chunk(b"fmt ", &fmt18)]);
        file.extend_from_slice(&w64_guid(b"junk"));
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(&w64_chunk(b"data", &[0; 2]));
        fill_w64_size(&mut file);
        assert_eq!(header(file), ErrorKind::InvalidHeader);
    }
}

#[test]
fn fmt_errors_carry_the_offset_of_the_chunk() {
    let at = |file: Vec<u8>| {
        let err = open(file).unwrap_err();
        (err.kind, err.byte_offset)
    };
    let invalid = |offset| (ErrorKind::InvalidHeader, Some(offset));

    // RIFF header 12 bytes, chunk header 8: the payload starts at 20.
    let empty = riff(&[chunk(b"fmt ", &[]), chunk(b"data", &data16())]);
    assert_eq!(at(empty), invalid(20));
    let no_channels = riff(&[
        chunk(b"fmt ", &fmt(1, 0, 8000, 2, 16)),
        chunk(b"data", &data16()),
    ]);
    assert_eq!(at(no_channels), invalid(20));
    let unknown = riff(&[
        chunk(b"fmt ", &fmt(0x1234, 2, 8000, 4, 16)),
        chunk(b"data", &data16()),
    ]);
    assert_eq!(at(unknown), (ErrorKind::UnsupportedCodec, Some(20)));

    // After a 12-byte JUNK chunk, and in Wave64 (header 40, chunk header 24).
    let after_junk = riff(&[
        chunk(b"JUNK", &[0; 4]),
        chunk(b"fmt ", &fmt(1, 0, 8000, 2, 16)),
        chunk(b"data", &data16()),
    ]);
    assert_eq!(at(after_junk), invalid(32));
    let wave64 = w64(&[
        w64_chunk(b"fmt ", &fmt(1, 0, 8000, 2, 16)),
        w64_chunk(b"data", &data16()),
    ]);
    assert_eq!(at(wave64), invalid(64));
}
