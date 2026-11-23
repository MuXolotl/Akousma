//! `fmt ` quirks, extensible GUIDs, every PCM width, floats and G.711.

use akousma_core::pcm::{ALAW_TO_I16, MULAW_TO_I16};
use akousma_core::probe::Demuxer;
use akousma_core::{ChannelMask, CodecId, ErrorKind};

use crate::common::{chunk, chunk_be, decode, fmt, fmt_extensible, open, riff, subtype, wave};

fn file(fmt: &[u8], data: &[u8]) -> Vec<u8> {
    riff(&[chunk(b"fmt ", fmt), chunk(b"data", data)])
}

#[test]
fn fmt_quirks_and_extensible_guids() {
    // A 40-byte plain PCM `fmt ` with `cbSize` 0 and 24 bits.
    let mut long = fmt(1, 1, 48_000, 3, 24);
    long.extend_from_slice(&[0; 24]);
    let demuxer = open(file(&long, &[0, 0, 0x80])).unwrap();
    assert_eq!(demuxer.tracks()[0].params.codec, CodecId::PcmS24Le);
    assert_eq!(
        decode::<i32>(file(&long, &[0, 0, 0x80])).unwrap()[0],
        [i32::MIN]
    );

    // Valid bits 0: the container width.
    let ext = fmt_extensible(2, 48_000, 8, 32, 0, 0x3, subtype(1));
    let params = open(file(&ext, &[0; 8])).unwrap().tracks()[0]
        .params
        .clone();
    assert_eq!(
        (params.codec, params.bits_per_sample),
        (CodecId::PcmS32Le, 32)
    );
    assert_eq!(params.channel_mask, ChannelMask::STEREO);

    // Valid bits above the container: invalid.
    let ext = fmt_extensible(2, 48_000, 4, 16, 20, 0x3, subtype(1));
    assert_eq!(
        open(file(&ext, &[0; 4])).unwrap_err().kind,
        ErrorKind::InvalidHeader
    );

    // A partial mask (front left and right of center on 3 channels) is kept.
    let ext = fmt_extensible(3, 48_000, 6, 16, 16, 0x40 | 0x80, subtype(1));
    let mask = open(file(&ext, &[0; 6])).unwrap().tracks()[0]
        .params
        .channel_mask;
    assert_eq!(mask, ChannelMask::from_raw(0xC0));
    // More speakers than channels: no mask.
    let ext = fmt_extensible(2, 48_000, 4, 16, 16, 0x3F, subtype(1));
    let mask = open(file(&ext, &[0; 4])).unwrap().tracks()[0]
        .params
        .channel_mask;
    assert_eq!(mask, ChannelMask::EMPTY);
    // `SPEAKER_ALL` and other high bits are dropped.
    let ext = fmt_extensible(2, 48_000, 4, 16, 16, 0x8000_0003, subtype(1));
    let mask = open(file(&ext, &[0; 4])).unwrap().tracks()[0]
        .params
        .channel_mask;
    assert_eq!(mask, ChannelMask::STEREO);
    // Plain files: mono and stereo masks, none for more channels.
    for (channels, mask) in [
        (1, ChannelMask::MONO),
        (2, ChannelMask::STEREO),
        (3, ChannelMask::EMPTY),
    ] {
        let plain = fmt(1, channels, 8000, 2 * channels, 16);
        let got = open(file(&plain, &[0; 6])).unwrap().tracks()[0]
            .params
            .channel_mask;
        assert_eq!(got, mask);
    }

    // Ambisonic B-format GUID: PCM without a mask.
    let mut amb = subtype(1);
    amb[2..].copy_from_slice(&[
        0x00, 0x00, 0x21, 0x07, 0xD3, 0x11, 0x86, 0x44, 0xC8, 0xC1, 0xCA, 0x00, 0x00, 0x00,
    ]);
    let ext = fmt_extensible(4, 48_000, 8, 16, 16, 0, amb);
    let params = open(file(&ext, &[0; 8])).unwrap().tracks()[0]
        .params
        .clone();
    assert_eq!(
        (params.codec, params.channel_mask),
        (CodecId::PcmS16Le, ChannelMask::EMPTY)
    );

    // A vendor GUID that starts with the PCM tag.
    let mut vendor = subtype(1);
    vendor[2..6].copy_from_slice(&[0xAD, 0xDE, 0xEF, 0xBE]);
    let ext = fmt_extensible(2, 48_000, 4, 16, 16, 0x3, vendor);
    assert_eq!(
        open(file(&ext, &[0; 4])).unwrap_err().kind,
        ErrorKind::UnsupportedCodec
    );

    // Extensible `fmt ` shorter than 40 bytes or with a small `cbSize`.
    let ext = fmt_extensible(2, 48_000, 4, 16, 16, 0x3, subtype(1));
    assert_eq!(
        open(file(&ext[..38], &[0; 4])).unwrap_err().kind,
        ErrorKind::InvalidHeader
    );
    let mut small = ext.clone();
    small[16] = 20;
    assert_eq!(
        open(file(&small, &[0; 4])).unwrap_err().kind,
        ErrorKind::InvalidHeader
    );

    // Channel counts are checked on the 16-bit field.
    for channels in [0, 65, 257] {
        let bad = fmt(1, channels, 8000, 2, 16);
        assert_eq!(
            open(file(&bad, &[0; 4])).unwrap_err().kind,
            ErrorKind::InvalidHeader
        );
    }
    let bad = fmt(1, 1, 7_680_001, 2, 16);
    assert_eq!(
        open(file(&bad, &[0; 4])).unwrap_err().kind,
        ErrorKind::InvalidHeader
    );

    // RIFX knows only PCM, float and G.711.
    for tag in [0xFFFEu16, 0x0002, 0x0011] {
        let mut be = Vec::new();
        for field in [tag, 1] {
            be.extend_from_slice(&field.to_be_bytes());
        }
        be.extend_from_slice(&8000u32.to_be_bytes());
        be.extend_from_slice(&0u32.to_be_bytes());
        be.extend_from_slice(&256u16.to_be_bytes());
        be.extend_from_slice(&4u16.to_be_bytes());
        be.resize(40, 0);
        let rifx = wave(
            b"RIFX",
            &[chunk_be(b"fmt ", &be), chunk_be(b"data", &[0; 4])],
        );
        assert_eq!(open(rifx).unwrap_err().kind, ErrorKind::UnsupportedCodec);
    }

    // Unknown tags and float widths.
    assert_eq!(
        open(file(&fmt(0x1234, 1, 8000, 2, 16), &[0; 2]))
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedCodec
    );
    assert_eq!(
        open(file(&fmt(3, 1, 8000, 2, 16), &[0; 2]))
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedFeature
    );

    // Compressed streams are passed on with their extension and delay.
    let mut mp3 = fmt(0x55, 2, 44_100, 1, 0);
    mp3.extend_from_slice(&12u16.to_le_bytes());
    mp3.extend_from_slice(&[1, 0, 2, 0, 0, 0, 0x40, 0x01, 0x01, 0x00, 0x71, 0x05]);
    let params = open(file(&mp3, &[0xFF; 10])).unwrap().tracks()[0]
        .params
        .clone();
    assert_eq!(
        (params.codec, params.delay, params.total_frames),
        (CodecId::Mp3, 0x0571, None)
    );
    assert_eq!(params.extra_data.len(), 12);
}

fn decode_pcm(tag: u16, channels: u16, align: u16, bits: u16, data: &[u8]) -> (CodecId, Vec<u8>) {
    let f = fmt(tag, channels, 8000, align, bits);
    let codec = open(file(&f, data)).unwrap().tracks()[0].params.codec;
    (codec, file(&f, data))
}

#[test]
fn pcm_all_depths_and_ieee_float() {
    // 8-bit unsigned.
    let (codec, f) = decode_pcm(1, 1, 1, 8, &[0, 128, 255]);
    assert_eq!(codec, CodecId::PcmU8);
    assert_eq!(decode::<i16>(f).unwrap()[0], [-32768, 0, 32512]);
    // 12-bit in 2 bytes.
    let (codec, f) = decode_pcm(1, 1, 2, 12, &[0xF0, 0x7F]);
    assert_eq!(codec, CodecId::PcmS16Le);
    assert_eq!(decode::<i16>(f).unwrap()[0], [0x7FF0]);
    // 16-bit little-endian, with `i16::MIN`.
    let (codec, f) = decode_pcm(1, 1, 2, 16, &[0x00, 0x80, 0x34, 0x12]);
    assert_eq!(codec, CodecId::PcmS16Le);
    assert_eq!(decode::<i16>(f).unwrap()[0], [i16::MIN, 0x1234]);
    // 20-bit in 3 and in 4 bytes.
    let (codec, f) = decode_pcm(1, 1, 3, 20, &[0x00, 0x00, 0x80]);
    assert_eq!(codec, CodecId::PcmS24Le);
    assert_eq!(decode::<i32>(f).unwrap()[0], [i32::MIN]);
    let (codec, f) = decode_pcm(1, 1, 4, 20, &[0x00, 0x10, 0x00, 0x80]);
    assert_eq!(codec, CodecId::PcmS32Le);
    assert_eq!(decode::<i32>(f).unwrap()[0], [i32::MIN + 0x1000]);
    // 24-bit: `i24::MIN` and `i24::MAX`, MSB-aligned.
    let (codec, f) = decode_pcm(1, 2, 6, 24, &[0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F]);
    assert_eq!(codec, CodecId::PcmS24Le);
    let planes = decode::<i32>(f).unwrap();
    assert_eq!((planes[0][0], planes[1][0]), (i32::MIN, 0x7FFF_FF00));
    // 32-bit.
    let (codec, f) = decode_pcm(1, 1, 4, 32, &i32::MIN.to_le_bytes());
    assert_eq!(codec, CodecId::PcmS32Le);
    assert_eq!(decode::<i32>(f).unwrap()[0], [i32::MIN]);
    // 64-bit: the top 32 bits.
    let (codec, f) = decode_pcm(1, 1, 8, 64, &0x1234_5678_9ABC_DEF0i64.to_le_bytes());
    assert_eq!(codec, CodecId::PcmS64Le);
    assert_eq!(decode::<i32>(f).unwrap()[0], [0x1234_5678]);

    // Big-endian widths in RIFX.
    for (bits, sample, expected) in [
        (16u16, vec![0x80, 0x00], i32::from(i16::MIN)),
        (24, vec![0x80, 0x00, 0x00], i32::MIN),
        (32, vec![0x80, 0x00, 0x00, 0x01], i32::MIN + 1),
        (64, vec![0x80, 0, 0, 0, 0, 0, 0, 0], i32::MIN),
    ] {
        let width = bits / 8;
        let mut be = Vec::new();
        for field in [1u16, 1] {
            be.extend_from_slice(&field.to_be_bytes());
        }
        be.extend_from_slice(&8000u32.to_be_bytes());
        be.extend_from_slice(&(8000 * u32::from(width)).to_be_bytes());
        be.extend_from_slice(&width.to_be_bytes());
        be.extend_from_slice(&bits.to_be_bytes());
        let rifx = wave(
            b"RIFX",
            &[chunk_be(b"fmt ", &be), chunk_be(b"data", &sample)],
        );
        let got = if bits == 16 {
            i32::from(decode::<i16>(rifx).unwrap()[0][0])
        } else {
            decode::<i32>(rifx).unwrap()[0][0]
        };
        assert_eq!(got, expected, "{bits} bits");
    }

    // Floats: peaks above 1.0 are kept, NaN and infinities replaced.
    let values = [
        1.5f32,
        -2.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        0.25,
    ];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (codec, f) = decode_pcm(3, 1, 4, 32, &data);
    assert_eq!(codec, CodecId::PcmF32Le);
    assert_eq!(
        decode::<f32>(f).unwrap()[0],
        [1.5, -2.0, 0.0, 1.0, -1.0, 0.25]
    );
    let values = [3.0f64, f64::NAN, -0.5];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (codec, f) = decode_pcm(3, 1, 8, 64, &data);
    assert_eq!(codec, CodecId::PcmF64Le);
    assert_eq!(decode::<f32>(f).unwrap()[0], [3.0, 0.0, -0.5]);

    // No samples, one sample.
    assert!(decode::<i16>(file(&fmt(1, 1, 8000, 2, 16), &[])).unwrap()[0].is_empty());
    assert_eq!(
        decode::<i16>(file(&fmt(1, 1, 8000, 2, 16), &[1, 0])).unwrap()[0],
        [1]
    );
}

#[test]
fn g711_alaw_and_mulaw_bit_exact() {
    let all: Vec<u8> = (0..=255).collect();
    for (tag, table) in [(6u16, &ALAW_TO_I16), (7, &MULAW_TO_I16)] {
        // One byte per sample, also with the decoded width of 16 bits in
        // the header.
        for (align, bits) in [(1, 8), (2, 16)] {
            let f = file(&fmt(tag, 1, 8000, align, bits), &all);
            assert_eq!(open(f.clone()).unwrap().tracks()[0].params.block_align, 1);
            assert_eq!(decode::<i16>(f).unwrap()[0], table.to_vec());
        }
        // Any other block alignment is invalid.
        let f = file(&fmt(tag, 1, 8000, 3, 8), &all);
        assert_eq!(open(f).unwrap_err().kind, ErrorKind::InvalidHeader);
    }
    // Reference points of ITU-T G.711.
    assert_eq!(
        (ALAW_TO_I16[0x00], ALAW_TO_I16[0x55], ALAW_TO_I16[0xD5]),
        (-5504, -8, 8)
    );
    assert_eq!(
        (MULAW_TO_I16[0x00], MULAW_TO_I16[0x7F], MULAW_TO_I16[0xFF]),
        (-32124, 0, 0)
    );
}
