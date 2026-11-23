//! Decodes a WAVE file packet by packet and prints its format, length and
//! peak level.
//!
//! ```text
//! cargo run --example decode_wav -- path/to/file.wav
//! ```
//!
//! Without an argument, a generated one-second tone is decoded.

use std::fs::File;
use std::io::{BufReader, Cursor};

use akousma::io::MediaSource;
use akousma::{AudioBuf, ChannelMask, DecodeOptions, DemuxOptions, Error, Hint, Packet};

/// A one-second 440 Hz 16-bit mono tone at 8 kHz.
fn tone() -> Vec<u8> {
    let rate = 8_000u32;
    let data: Vec<u8> = (0..rate)
        .map(|n| {
            let phase = 2.0 * std::f32::consts::PI * 440.0 * n as f32 / rate as f32;
            (phase.sin() * 16_000.0) as i16
        })
        .flat_map(i16::to_le_bytes)
        .collect();
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00");
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(b"\x02\x00\x10\x00data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    wav
}

fn run(source: Box<dyn MediaSource>) -> Result<(), Error> {
    let probed = akousma::default_probe().open(
        &Hint::new().with_extension("wav"),
        source,
        &DemuxOptions::new(),
    )?;
    let mut demuxer = probed.demuxer;
    let track = demuxer.default_track().expect("one track").clone();
    let params = &track.params;
    println!(
        "{}: {:?}, {} Hz, {} channels, {} bits",
        probed.format, params.codec, params.sample_rate, params.channels, params.bits_per_sample
    );

    let mut decoder = akousma::default_codecs().make(params, &DecodeOptions::new())?;
    // The decoder reshapes the buffer to the stream on the first packet.
    let mut buf = AudioBuf::f32(1, params.sample_rate, ChannelMask::EMPTY)?;
    let mut packet = Packet::default();
    let mut interleaved = Vec::new();
    let (mut frames, mut peak) = (0usize, 0f32);
    while demuxer.next_packet(&mut packet)? {
        decoder.decode(&packet, &mut buf)?;
        interleaved.resize(buf.samples(), 0.0);
        buf.copy_interleaved_f32(&mut interleaved);
        frames += buf.frames();
        peak = interleaved.iter().fold(peak, |peak, s| peak.max(s.abs()));
    }
    let seconds = frames as f64 / f64::from(params.sample_rate);
    println!("{frames} frames ({seconds:.3} s), peak {peak:.3}");
    for revision in demuxer.metadata().iter() {
        for tag in &revision.tags {
            println!("{}: {:?}", tag.raw.key, tag.raw.value);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source: Box<dyn MediaSource> = match std::env::args_os().nth(1) {
        Some(path) => Box::new(BufReader::new(File::open(path)?)),
        None => Box::new(Cursor::new(tone())),
    };
    run(source)?;
    Ok(())
}
