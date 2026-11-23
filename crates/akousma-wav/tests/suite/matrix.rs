//! The five containers crossed with the codecs they can carry.
//!
//! Each format has a home container elsewhere in this suite; here the same
//! format travels through a container whose header is read with different
//! endianness or alignment, so that a mistake in one of the two cannot hide
//! behind the pairing the rest of the suite uses.

use akousma_core::CodecId;
use akousma_core::pcm::{ALAW_TO_I16, MULAW_TO_I16};
use akousma_core::probe::Demuxer;

use crate::adpcm::{ima_block, ima_data};
use crate::common::{chunk, chunk_be, decode, fmt, fmt_extensible, open, riff, subtype, wave};
use crate::containers::{ds64, raw_chunk, w64, w64_chunk};

/// A 16-byte `fmt ` payload with big-endian fields, for RIFX.
fn fmt_be(tag: u16, channels: u16, rate: u32, block_align: u16, bits: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for field in [tag, channels] {
        out.extend_from_slice(&field.to_be_bytes());
    }
    out.extend_from_slice(&rate.to_be_bytes());
    out.extend_from_slice(&(rate * u32::from(block_align)).to_be_bytes());
    out.extend_from_slice(&block_align.to_be_bytes());
    out.extend_from_slice(&bits.to_be_bytes());
    out
}

fn codec_of(bytes: Vec<u8>) -> CodecId {
    open(bytes).unwrap().tracks()[0].params.codec
}

#[test]
fn rifx_carries_big_endian_ieee_float() {
    // 32-bit: the bytes are reversed with respect to the little-endian file
    // of the same value.
    let values = [1.5f32, -2.0, f32::NAN, 0.25];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
    let file = wave(
        b"RIFX",
        &[
            chunk_be(b"fmt ", &fmt_be(3, 1, 8000, 4, 32)),
            chunk_be(b"data", &data),
        ],
    );
    assert_eq!(codec_of(file.clone()), CodecId::PcmF32Be);
    // NaN becomes 0, as in the little-endian file.
    assert_eq!(decode::<f32>(file).unwrap()[0], [1.5, -2.0, 0.0, 0.25]);

    // 64-bit, narrowed to `f32` planes.
    let values = [3.0f64, -0.5];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
    let file = wave(
        b"RIFX",
        &[
            chunk_be(b"fmt ", &fmt_be(3, 1, 8000, 8, 64)),
            chunk_be(b"data", &data),
        ],
    );
    assert_eq!(codec_of(file.clone()), CodecId::PcmF64Be);
    assert_eq!(decode::<f32>(file).unwrap()[0], [3.0, -0.5]);
}

#[test]
fn wave64_carries_float_g711_and_s24() {
    let values = [1.5f32, -0.25, 2.0];
    let data: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let file = w64(&[
        w64_chunk(b"fmt ", &fmt(3, 1, 8000, 4, 32)),
        w64_chunk(b"data", &data),
    ]);
    assert_eq!(codec_of(file.clone()), CodecId::PcmF32Le);
    assert_eq!(decode::<f32>(file).unwrap()[0], [1.5, -0.25, 2.0]);

    // 24-bit keeps the MSB-aligned `i32` planes.
    let file = w64(&[
        w64_chunk(b"fmt ", &fmt(1, 2, 8000, 6, 24)),
        w64_chunk(b"data", &[0x00, 0x00, 0x80, 0xFF, 0xFF, 0x7F]),
    ]);
    assert_eq!(codec_of(file.clone()), CodecId::PcmS24Le);
    let planes = decode::<i32>(file).unwrap();
    assert_eq!((planes[0][0], planes[1][0]), (i32::MIN, 0x7FFF_FF00));

    // G.711: the code bytes are the same in every container.
    let all: Vec<u8> = (0..=255).collect();
    for (tag, table, codec) in [
        (6u16, &ALAW_TO_I16, CodecId::G711Alaw),
        (7, &MULAW_TO_I16, CodecId::G711Mulaw),
    ] {
        let file = w64(&[
            w64_chunk(b"fmt ", &fmt(tag, 1, 8000, 1, 8)),
            w64_chunk(b"data", &all),
        ]);
        assert_eq!(codec_of(file.clone()), codec);
        assert_eq!(decode::<i16>(file).unwrap()[0], table.to_vec());
    }
}

#[test]
fn wave64_carries_ima_adpcm() {
    const SPB: u16 = 65;
    const ALIGN: usize = 36;
    let mut f = fmt(0x11, 1, 8000, ALIGN as u16, 4);
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&SPB.to_le_bytes());
    let data = ima_data(1, ALIGN, 3, 0x51AB);
    let file = w64(&[w64_chunk(b"fmt ", &f), w64_chunk(b"data", &data)]);
    let mut demuxer = open(file).unwrap();
    assert_eq!(demuxer.tracks()[0].params.codec, CodecId::AdpcmImaWav);
    assert_eq!(
        demuxer.tracks()[0].params.total_frames,
        Some(3 * u64::from(SPB))
    );
    let planes = crate::common::drain::<i16>(&mut demuxer).unwrap();
    let expected: Vec<i16> = data
        .chunks(ALIGN)
        .flat_map(|block| ima_block(block, 1, usize::from(SPB))[0].clone())
        .collect();
    assert_eq!(planes[0], expected);
}

#[test]
fn wave64_carries_wave_format_extensible() {
    // 5.1 in a 40-byte `fmt `: the mask names all six speakers.
    let guid = subtype(1);
    let f = fmt_extensible(6, 48000, 12, 16, 16, 0x3F, guid);
    assert_eq!(f.len(), 40);
    let samples: [i16; 12] = [-1, -2, -3, -4, -5, -6, 1, 2, 3, 4, 5, 6];
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let file = w64(&[w64_chunk(b"fmt ", &f), w64_chunk(b"data", &data)]);
    let demuxer = open(file.clone()).unwrap();
    let params = &demuxer.tracks()[0].params;
    assert_eq!((params.codec, params.channels), (CodecId::PcmS16Le, 6));
    assert_eq!(params.channel_mask.count(), 6);
    let planes = decode::<i16>(file).unwrap();
    let first: Vec<i16> = planes.iter().map(|plane| plane[0]).collect();
    assert_eq!(first, [-1, -2, -3, -4, -5, -6]);
}

#[test]
fn rf64_honours_the_ds64_sample_count_of_adpcm() {
    // RF64 keeps `sampleCount64` as the replacement of `fact`; BW64 drops the
    // field. Ten frames of a 65-frame block: the rest is padding.
    const SPB: u16 = 65;
    const ALIGN: usize = 36;
    let mut f = fmt(0x11, 1, 8000, ALIGN as u16, 4);
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&SPB.to_le_bytes());
    let data = ima_data(1, ALIGN, 1, 0x7E57);
    let file = |magic: &[u8; 4]| {
        wave(
            magic,
            &[
                chunk(b"ds64", &ds64(ALIGN as u64, 10)),
                chunk(b"fmt ", &f),
                raw_chunk(b"data", u32::MAX, &data),
            ],
        )
    };

    let rf64 = file(b"RF64");
    let demuxer = open(rf64.clone()).unwrap();
    assert_eq!(demuxer.tracks()[0].params.total_frames, Some(10));
    let planes = decode::<i16>(rf64).unwrap();
    assert_eq!(
        planes[0],
        ima_block(&data, 1, usize::from(SPB))[0][..10].to_vec()
    );

    // The same file as BW64 keeps the whole block: the third `ds64` field is
    // not a sample count there.
    let bw64 = file(b"BW64");
    let demuxer = open(bw64.clone()).unwrap();
    assert_eq!(
        demuxer.tracks()[0].params.total_frames,
        Some(u64::from(SPB))
    );
    assert_eq!(decode::<i16>(bw64).unwrap()[0].len(), usize::from(SPB));
}

#[test]
fn extensible_valid_bits_below_the_container_stay_msb_aligned() {
    // The specification stores the samples in the most significant bits of
    // the container. A decoder that forgot this would return the same sound
    // 256 times quieter, so the planes are checked against the shift of
    // `bits_per_sample`.
    let cases: [(u16, u16, u64, u32); 2] = [
        // (container bits, valid bits, stored sample, restored sample)
        (32, 24, 0x1234_5600, 0x0012_3456),
        (24, 20, 0x00AB_CDE0, 0x000A_BCDE),
    ];
    for (container, valid, stored, restored) in cases {
        let width = container / 8;
        let ext = fmt_extensible(1, 48_000, width, container, valid, 0x4, subtype(1));
        let data = stored.to_le_bytes()[..usize::from(width)].to_vec();
        let file = riff(&[chunk(b"fmt ", &ext), chunk(b"data", &data)]);
        let demuxer = open(file.clone()).unwrap();
        let params = &demuxer.tracks()[0].params;
        assert_eq!(
            params.bits_per_sample, valid as u8,
            "{valid} of {container}"
        );
        // The plane is the whole container, MSB-first: the 24-bit word of a
        // 24-bit container moves up by 8 bits.
        let planes = decode::<i32>(file).unwrap();
        assert_eq!(
            planes[0][0] as u32,
            (stored as u32) << (32 - u32::from(container)),
            "{valid} of {container}"
        );
        assert_eq!(
            (planes[0][0] as u32) >> (32 - u32::from(valid)),
            restored,
            "{valid} of {container}"
        );
    }
}
