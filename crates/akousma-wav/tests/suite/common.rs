//! Builders of synthetic WAVE files and a decode loop shared by the tests.

use std::io::Cursor;

use akousma_core::audio::{AudioBuf, AudioPlanes, Sample};
use akousma_core::codec::{CodecParams, CodecRegistry, Packet};
use akousma_core::probe::Demuxer;
use akousma_core::{ChannelMask, DecodeOptions, DemuxOptions, Error};
use akousma_wav::WavDemuxer;

/// A RIFF chunk: identifier, little-endian size, payload, pad byte.
pub fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A RIFX chunk: big-endian size.
pub fn chunk_be(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A stream header with `magic` and the given chunks.
pub fn wave(magic: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut out = magic.to_vec();
    let size = (body.len() + 4) as u32;
    if magic == b"RIFX" {
        out.extend_from_slice(&size.to_be_bytes());
    } else {
        out.extend_from_slice(&size.to_le_bytes());
    }
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(&body);
    out
}

/// A RIFF file.
pub fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    wave(b"RIFF", chunks)
}

/// A 16-byte `fmt ` payload, little-endian.
pub fn fmt(tag: u16, channels: u16, rate: u32, block_align: u16, bits: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out
}

/// `KSDATAFORMAT_SUBTYPE_*` GUID of a format tag.
pub fn subtype(tag: u16) -> [u8; 16] {
    let mut guid = [
        0, 0, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, 0, 0,
    ];
    guid.copy_within(0..14, 2);
    guid[..2].copy_from_slice(&tag.to_le_bytes());
    guid
}

/// A 40-byte `WAVE_FORMAT_EXTENSIBLE` payload.
pub fn fmt_extensible(
    channels: u16,
    rate: u32,
    block_align: u16,
    bits: u16,
    valid_bits: u16,
    mask: u32,
    guid: [u8; 16],
) -> Vec<u8> {
    let mut out = fmt(0xFFFE, channels, rate, block_align, bits);
    out.extend_from_slice(&22u16.to_le_bytes());
    out.extend_from_slice(&valid_bits.to_le_bytes());
    out.extend_from_slice(&mask.to_le_bytes());
    out.extend_from_slice(&guid);
    out
}

/// A 16-bit PCM file with interleaved `samples`.
pub fn pcm16(channels: u16, samples: &[i16]) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    riff(&[
        chunk(b"fmt ", &fmt(1, channels, 8000, 2 * channels, 16)),
        chunk(b"data", &data),
    ])
}

/// Opens `bytes` with `options`.
pub fn open_with(
    bytes: Vec<u8>,
    options: &DemuxOptions,
) -> Result<WavDemuxer<Cursor<Vec<u8>>>, Error> {
    WavDemuxer::open(Cursor::new(bytes), options)
}

/// Opens `bytes` with the default options.
pub fn open(bytes: Vec<u8>) -> Result<WavDemuxer<Cursor<Vec<u8>>>, Error> {
    open_with(bytes, &DemuxOptions::new())
}

/// The registry with the decoders of the crate.
pub fn registry() -> CodecRegistry {
    let mut registry = CodecRegistry::new();
    akousma_wav::register_codecs(&mut registry);
    registry
}

/// Decodes every remaining packet of `demuxer` and returns the planes.
pub fn drain<T: Sample>(demuxer: &mut dyn Demuxer) -> Result<Vec<Vec<T>>, Error> {
    let params: CodecParams = demuxer.tracks()[0].params.clone();
    let mut decoder = registry().make(&params, &DecodeOptions::new())?;
    let channels = usize::from(params.channels);
    let mut out = vec![Vec::new(); channels];
    let mut packet = Packet::default();
    let mut buf = AudioBuf::new(
        AudioPlanes::I16(vec![Vec::new()]),
        1,
        ChannelMask::EMPTY,
        16,
    )?;
    while demuxer.next_packet(&mut packet)? {
        decoder.decode(&packet, &mut buf)?;
        for (c, plane) in out.iter_mut().enumerate() {
            plane.extend_from_slice(buf.plane::<T>(c).expect("sample type"));
        }
    }
    Ok(out)
}

/// Opens and decodes `bytes`.
pub fn decode<T: Sample>(bytes: Vec<u8>) -> Result<Vec<Vec<T>>, Error> {
    drain(&mut open(bytes)?)
}
