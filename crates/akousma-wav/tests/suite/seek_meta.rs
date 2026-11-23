//! Seeking and metadata chunks.

use std::time::Duration;

use akousma_core::codec::Packet;
use akousma_core::meta::{RawValue, StandardTag};
use akousma_core::probe::Demuxer;
use akousma_core::{DemuxOptions, ErrorKind, SeekMode, SeekTarget};

use crate::common::{chunk, drain, fmt, open, open_with, pcm16, riff};

fn frame(frame: u64) -> SeekTarget {
    SeekTarget::Frame { track_id: 0, frame }
}

/// An IMA ADPCM file of 4 mono blocks of 36 bytes (65 frames each), and a
/// Microsoft ADPCM file of 4 mono blocks of 20 bytes (28 frames each).
fn adpcm_files() -> [(Vec<u8>, u64); 2] {
    let body: Vec<u8> = (0..144u32).map(|i| (i * 37 % 251) as u8).collect();
    let mut ima = body.clone();
    for block in ima.chunks_mut(36) {
        block[2] %= 89;
    }
    let mut ms = body[..80].to_vec();
    for block in ms.chunks_mut(20) {
        block[0] %= 7;
    }
    [
        (
            riff(&[
                chunk(b"fmt ", &fmt(0x11, 1, 8000, 36, 4)),
                chunk(b"data", &ima),
            ]),
            65,
        ),
        (riff(&[chunk(b"fmt ", &ms_fmt()), chunk(b"data", &ms)]), 28),
    ]
}

/// `fmt ` of mono Microsoft ADPCM in blocks of 20 bytes, with 28 samples
/// per block and the seven standard coefficient pairs.
fn ms_fmt() -> Vec<u8> {
    let mut out = fmt(0x02, 1, 8000, 20, 4);
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&28u16.to_le_bytes());
    out.extend_from_slice(&7u16.to_le_bytes());
    for coeff in [
        256, 0, 512, -256, 0, 0, 192, 64, 240, 0, 460, -208, 392, -232i16,
    ] {
        out.extend_from_slice(&coeff.to_le_bytes());
    }
    out
}

#[test]
fn seek_o1_pcm_and_adpcm() {
    // PCM: both modes land on the target, forward and backward.
    let samples: Vec<i16> = (0..10_000).map(|i| (i * 7) as i16).collect();
    let mut demuxer = open(pcm16(2, &samples)).unwrap();
    let full = drain::<i16>(&mut demuxer).unwrap();
    for (mode, target) in [
        (SeekMode::Accurate, 4321),
        (SeekMode::Coarse, 17),
        (SeekMode::Accurate, 4999),
        (SeekMode::Coarse, 0),
    ] {
        let seeked = demuxer.seek(mode, frame(target)).unwrap();
        assert_eq!((seeked.required_ts, seeked.actual_ts), (target, target));
        let rest = drain::<i16>(&mut demuxer).unwrap();
        assert_eq!(rest[1], full[1][target as usize..]);
    }
    // By time: 0.5 s at 8 kHz.
    let time = SeekTarget::Time {
        track_id: None,
        time: Duration::from_millis(500),
    };
    assert_eq!(
        demuxer.seek(SeekMode::Accurate, time).unwrap().actual_ts,
        4000
    );
    // The end, and past it.
    for target in [5000, u64::MAX] {
        let seeked = demuxer.seek(SeekMode::Accurate, frame(target)).unwrap();
        assert_eq!((seeked.required_ts, seeked.actual_ts), (target, 5000));
        assert!(drain::<i16>(&mut demuxer).unwrap()[0].is_empty());
    }
    // Other tracks.
    let other = SeekTarget::Frame {
        track_id: 1,
        frame: 0,
    };
    assert_eq!(
        demuxer.seek(SeekMode::Accurate, other).unwrap_err().kind,
        ErrorKind::TrackNotFound
    );

    // ADPCM: one block seek; accurate seeks trim the start of the block.
    for (file, spb) in adpcm_files() {
        let mut demuxer = open(file).unwrap();
        let full = drain::<i16>(&mut demuxer).unwrap();
        let total = full[0].len() as u64;
        assert_eq!(total, 4 * spb);
        for target in [spb + 5, 3, 3 * spb, total - 1, 0] {
            let seeked = demuxer.seek(SeekMode::Accurate, frame(target)).unwrap();
            assert_eq!(seeked.actual_ts, target);
            assert_eq!(
                drain::<i16>(&mut demuxer).unwrap()[0],
                full[0][target as usize..]
            );

            let seeked = demuxer.seek(SeekMode::Coarse, frame(target)).unwrap();
            let start = target / spb * spb;
            assert_eq!(seeked.actual_ts, start);
            assert_eq!(
                drain::<i16>(&mut demuxer).unwrap()[0],
                full[0][start as usize..]
            );
        }
        let seeked = demuxer.seek(SeekMode::Accurate, frame(u64::MAX)).unwrap();
        assert_eq!(seeked.actual_ts, total);
        assert!(drain::<i16>(&mut demuxer).unwrap()[0].is_empty());
    }
}

/// Length, `pts`, duration and `trim_start` of the next packet.
fn next(demuxer: &mut dyn Demuxer) -> (usize, u64, u32, u32) {
    let mut packet = Packet::default();
    assert!(demuxer.next_packet(&mut packet).unwrap());
    (
        packet.data.len(),
        packet.pts,
        packet.duration,
        packet.trim_start,
    )
}

/// Mono IMA ADPCM of `blocks` blocks of `align` bytes.
fn ima_file(blocks: u32, align: u16) -> Vec<u8> {
    let len = blocks * u32::from(align);
    let mut body: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
    for block in body.chunks_mut(usize::from(align)) {
        block[2] %= 89;
    }
    riff(&[
        chunk(b"fmt ", &fmt(0x11, 1, 8000, align, 4)),
        chunk(b"data", &body),
    ])
}

/// Blocks per packet of the next packets until the end of the stream.
fn blocks_per_packet(demuxer: &mut dyn Demuxer, align: usize) -> Vec<usize> {
    let mut packet = Packet::default();
    let mut out = Vec::new();
    while demuxer.next_packet(&mut packet).unwrap() {
        out.push(packet.data.len() / align);
    }
    out
}

#[test]
fn adpcm_packets_after_a_seek_start_at_one_block_and_double() {
    // 100 blocks of 256 bytes: 505 frames per block and 16384 / 256 = 64
    // blocks per packet.
    let (spb, align) = (505u64, 256usize);
    let file = ima_file(100, 256);
    let mut demuxer = open(file.clone()).unwrap();
    // Reading without a seek takes full packets at once.
    assert_eq!(blocks_per_packet(&mut demuxer, align), [64, 36]);

    for (mode, target, trim) in [
        (SeekMode::Accurate, 10 * spb + 7, 7),
        (SeekMode::Coarse, 10 * spb + 7, 0),
    ] {
        demuxer.seek(mode, frame(target)).unwrap();
        assert_eq!(next(&mut demuxer), (align, 10 * spb, spb as u32, trim));
        let (len, pts, _, trim) = next(&mut demuxer);
        assert_eq!((len, pts, trim), (2 * align, 11 * spb, 0));
        // 4, 8, 16, 32, then the 27 blocks left, fewer than the usual 64.
        assert_eq!(blocks_per_packet(&mut demuxer, align), [4, 8, 16, 32, 27]);
    }
    // A new seek during the growth starts again at one block.
    demuxer.seek(SeekMode::Accurate, frame(0)).unwrap();
    assert_eq!(
        [next(&mut demuxer).0, next(&mut demuxer).0],
        [align, 2 * align]
    );
    demuxer
        .seek(SeekMode::Accurate, frame(3 * spb + 1))
        .unwrap();
    let after: Vec<usize> = blocks_per_packet(&mut demuxer, align);
    assert_eq!(after, [1, 2, 4, 8, 16, 32, 34]);
    // The last block: one packet of one block.
    demuxer
        .seek(SeekMode::Accurate, frame(99 * spb + 500))
        .unwrap();
    assert_eq!(next(&mut demuxer), (align, 99 * spb, spb as u32, 500));

    // The frames are those of a decode without a seek, from any target.
    let full = drain::<i16>(&mut open(file).unwrap()).unwrap();
    for target in [0, 1, spb - 1, spb, 63 * spb + 3, 64 * spb, 99 * spb + 504] {
        demuxer.seek(SeekMode::Accurate, frame(target)).unwrap();
        let rest = drain::<i16>(&mut demuxer).unwrap();
        assert_eq!(rest[0], full[0][target as usize..]);
        demuxer.seek(SeekMode::Coarse, frame(target)).unwrap();
        let rest = drain::<i16>(&mut demuxer).unwrap();
        assert_eq!(rest[0], full[0][(target / spb * spb) as usize..]);
    }
}

#[test]
fn adpcm_packets_stop_growing_at_the_usual_size() {
    // Blocks of 2304 bytes: 16384 / 2304 = 7 blocks per packet, not a power
    // of two; the growth stops at 7, never above.
    let mut demuxer = open(ima_file(30, 2304)).unwrap();
    demuxer.seek(SeekMode::Accurate, frame(5)).unwrap();
    assert_eq!(blocks_per_packet(&mut demuxer, 2304), [1, 2, 4, 7, 7, 7, 2]);
}

#[test]
fn pcm_packets_after_a_seek_keep_their_size() {
    // 20000 stereo 16-bit frames: 4096 frames, 16384 bytes, per packet.
    let samples: Vec<i16> = (0..40_000).map(|i| (i * 7) as i16).collect();
    let mut demuxer = open(pcm16(2, &samples)).unwrap();
    for (mode, target) in [(SeekMode::Accurate, 4321), (SeekMode::Coarse, 17)] {
        demuxer.seek(mode, frame(target)).unwrap();
        assert_eq!(next(&mut demuxer), (16_384, target, 4096, 0));
        assert_eq!(next(&mut demuxer), (16_384, target + 4096, 4096, 0));
    }
}

fn info() -> Vec<u8> {
    let mut list = b"INFO".to_vec();
    list.extend_from_slice(&chunk(b"INAM", b"Caf\xE9\0"));
    list.extend_from_slice(&chunk(b"IART", b"Artist\0"));
    chunk(b"LIST", &list)
}

fn id3(frame_id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut frame = frame_id.to_vec();
    frame.extend_from_slice(&(text.len() as u32 + 1).to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0]);
    frame.extend_from_slice(text.as_bytes());
    let mut tag = b"ID3\x03\x00\x00\x00\x00\x00".to_vec();
    tag.push(frame.len() as u8);
    tag.extend_from_slice(&frame);
    chunk(b"id3 ", &tag)
}

fn bext() -> Vec<u8> {
    let mut b = vec![0u8; 602];
    b[..11].copy_from_slice(b"Description");
    b[256..262].copy_from_slice(b"Studio");
    b[320..330].copy_from_slice(b"2026-11-17");
    b[338..346].copy_from_slice(&48_000u64.to_le_bytes());
    b[346..348].copy_from_slice(&2u16.to_le_bytes());
    b[348] = 0xAB;
    let loudness: [i16; 5] = [-2300, 0x7FFF, -100, -10_000, 500];
    for (i, v) in loudness.iter().enumerate() {
        b[412 + 2 * i..414 + 2 * i].copy_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(b"A=PCM,F=48000\r\n");
    chunk(b"bext", &b)
}

fn cue_and_adtl() -> Vec<u8> {
    let mut cue = 3u32.to_le_bytes().to_vec();
    for (id, offset, fcc) in [
        (1u32, 4000u32, b"data"),
        (2, 1000, b"data"),
        (3, 2000, b"slnt"),
    ] {
        cue.extend_from_slice(&id.to_le_bytes());
        cue.extend_from_slice(&0u32.to_le_bytes());
        cue.extend_from_slice(fcc);
        cue.extend_from_slice(&[0; 8]);
        cue.extend_from_slice(&offset.to_le_bytes());
    }
    // Room for a point more than declared, as some editors write.
    cue.extend_from_slice(&[0; 24]);
    let mut adtl = b"adtl".to_vec();
    adtl.extend_from_slice(&chunk(b"labl", b"\x02\x00\x00\x00Intro\0"));
    adtl.extend_from_slice(&chunk(b"note", b"\x02\x00\x00\x00Quiet\0"));
    let mut ltxt = 1u32.to_le_bytes().to_vec();
    ltxt.extend_from_slice(&500u32.to_le_bytes());
    ltxt.extend_from_slice(&[0; 12]);
    adtl.extend_from_slice(&chunk(b"ltxt", &ltxt));
    [chunk(b"cue ", &cue), chunk(b"LIST", &adtl)].concat()
}

fn sampler() -> Vec<u8> {
    let mut smpl = vec![0u8; 36 + 24];
    smpl[12..16].copy_from_slice(&60u32.to_le_bytes());
    smpl[28..32].copy_from_slice(&1u32.to_le_bytes());
    smpl[36 + 8..36 + 12].copy_from_slice(&100u32.to_le_bytes());
    smpl[36 + 12..36 + 16].copy_from_slice(&900u32.to_le_bytes());
    let mut acid = vec![0u8; 24];
    acid[12..16].copy_from_slice(&8u32.to_le_bytes());
    acid[16..18].copy_from_slice(&4u16.to_le_bytes());
    acid[18..20].copy_from_slice(&4u16.to_le_bytes());
    acid[20..24].copy_from_slice(&120.0f32.to_le_bytes());
    let inst = [60u8, 0xFE, 3, 0, 127, 1, 127];
    [
        chunk(b"smpl", &smpl),
        chunk(b"acid", &acid),
        chunk(b"inst", &inst),
        chunk(b"iXML", b"<BWFXML/>\0"),
    ]
    .concat()
}

#[test]
fn metadata_info_id3_bext_cue_chapters_smpl() {
    let data: Vec<u8> = vec![0; 2 * 8000];
    // Chunks before and after `data`.
    let file = riff(&[
        bext(),
        chunk(b"fmt ", &fmt(1, 1, 8000, 2, 16)),
        chunk(b"data", &data),
        info(),
        id3(b"TIT2", "One"),
        id3(b"TPE1", "Two"),
        cue_and_adtl(),
        sampler(),
    ]);
    let meta = DemuxOptions::new().with_read_metadata(true);
    let mut demuxer = open_with(file.clone(), &meta).unwrap();

    let chapters = demuxer.chapters().to_vec();
    assert_eq!(chapters.len(), 2);
    assert_eq!(
        (chapters[0].id.as_str(), chapters[0].start),
        ("2", Duration::from_millis(125))
    );
    assert_eq!(chapters[0].end, Some(Duration::from_millis(500)));
    assert_eq!(
        chapters[0].tags[0].standard,
        Some(StandardTag::TrackTitle("Intro".into()))
    );
    assert_eq!(
        chapters[0].tags[1].standard,
        Some(StandardTag::Comment("Quiet".into()))
    );
    assert_eq!(
        (chapters[1].start, chapters[1].end),
        (
            Duration::from_millis(500),
            Some(Duration::from_micros(562_500))
        )
    );

    // In chunk order: `bext` and `INFO`, the two `id3 ` chunks, then the
    // sampler chunks (the cue points and labels give no fields).
    let log = demuxer.metadata();
    assert_eq!(log.len(), 3);
    let revisions: Vec<_> = log.iter().cloned().collect();
    let find_in = |index: usize, key: &str| {
        revisions[index]
            .tags
            .iter()
            .find(|t| t.raw.key == key)
            .map(|t| t.raw.value.clone())
    };
    let find = |key: &str| find_in(0, key);
    let sampler = |key: &str| find_in(2, key);
    assert_eq!(
        find("BEXT:DESCRIPTION"),
        Some(RawValue::Text("Description".into()))
    );
    assert_eq!(
        find("BEXT:ORIGINATOR"),
        Some(RawValue::Text("Studio".into()))
    );
    assert_eq!(find("BEXT:TIME_REFERENCE"), Some(RawValue::Int(48_000)));
    assert_eq!(find("BEXT:LOUDNESS_VALUE"), Some(RawValue::Float(-23.0)));
    assert_eq!(find("BEXT:LOUDNESS_RANGE"), None);
    assert_eq!(
        find("BEXT:MAX_TRUE_PEAK_LEVEL"),
        Some(RawValue::Float(-1.0))
    );
    assert_eq!(find("BEXT:MAX_MOMENTARY_LOUDNESS"), None);
    assert_eq!(
        find("BEXT:MAX_SHORT_TERM_LOUDNESS"),
        Some(RawValue::Float(5.0))
    );
    assert!(
        matches!(find("BEXT:UMID"), Some(RawValue::Text(umid)) if umid.starts_with("AB00") && umid.len() == 128)
    );
    assert_eq!(
        find("BEXT:CODING_HISTORY"),
        Some(RawValue::Text("A=PCM,F=48000\r\n".into()))
    );
    assert_eq!(find("INAM"), Some(RawValue::Text("Caf\u{e9}".into())));
    assert_eq!(find("SMPL:MIDI_UNITY_NOTE"), None);
    assert_eq!(sampler("SMPL:MIDI_UNITY_NOTE"), Some(RawValue::Int(60)));
    assert_eq!(sampler("SMPL:LOOP_0_END"), Some(RawValue::Int(900)));
    assert_eq!(sampler("ACID:METER"), Some(RawValue::Text("4/4".into())));
    assert_eq!(sampler("INST:FINE_TUNE"), Some(RawValue::Int(-2)));
    assert_eq!(sampler("IXML"), Some(RawValue::Text("<BWFXML/>".into())));
    assert!(
        revisions[2]
            .tags
            .iter()
            .any(|t| t.standard == Some(StandardTag::Bpm(120.0)))
    );
    // Both `id3 ` chunks, which follow each other, in one revision.
    let keys: Vec<_> = revisions[1]
        .tags
        .iter()
        .map(|t| t.raw.key.as_str())
        .collect();
    assert_eq!(keys, ["TIT2", "TPE1"]);

    // Without metadata: nothing is read.
    let no_meta = DemuxOptions::new().with_read_metadata(false);
    let mut demuxer = open_with(file, &no_meta).unwrap();
    assert!(demuxer.metadata().is_empty());
    assert!(demuxer.chapters().is_empty());
    assert_eq!(drain::<i16>(&mut demuxer).unwrap()[0].len(), 8000);
}
