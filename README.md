# Akousma

Akousma is an audio decoding library written in Rust. It detects supported
formats, decodes audio to sample buffers, and reads tags, cover art and
chapters. The library has no third-party dependencies and forbids `unsafe`
code.

## Supported formats

| Container | Variants | Benchmarks | Comparison |
| :--- | :--- | :---: | :---: |
| WAVE | RIFF, RIFX, RF64, BW64, Sony Wave64 | [WAVE benchmarks](BENCHMARKS.md) | [WAVE comparison](COMPARISON.md) |

Codecs: integer PCM in 8-, 16-, 24-, 32- and 64-bit containers (WAVE also
accepts supported non-byte-aligned widths), IEEE float (32 or 64 bits), G.711
A-law and μ-law, IMA ADPCM, and Microsoft ADPCM. Integer decoding keeps at
most 32 bits of precision; 64-bit samples are reduced to their most
significant 32 bits.

Tags and metadata: ID3v1, ID3v2, APE, Lyrics3 v2.00 and WAVE metadata,
including cover art and chapters.

## Installation

```sh
cargo add akousma
```

The other crates can be used on their own:
`cargo add akousma-core`, `cargo add akousma-meta`, `cargo add akousma-wav`.

## Example

```rust
fn main() -> Result<(), akousma::Error> {
    let decoded = akousma::decode_file("song.wav")?;
    println!(
        "{}: {} Hz, {} channels, {} frames",
        decoded.format,
        decoded.params.sample_rate,
        decoded.params.channels,
        decoded.frames(),
    );
    for revision in &decoded.metadata {
        for tag in &revision.tags {
            println!("{}: {:?}", tag.raw.key, tag.raw.value);
        }
    }
    Ok(())
}
```

`decode_file` holds the whole track in memory. To stream, open a demuxer with
`akousma::default_probe()` and decode its packets with a decoder from
`akousma::default_codecs()`; see `crates/akousma/examples/decode_wav.rs`.
The source must be seekable. To decode a pipe or socket, first buffer its
contents into a seekable reader such as a `Cursor`.

## Streaming with native sample planes

`decode_file` collects the samples as `f32`, so 32- and 64-bit integer
sources and 64-bit float sources can lose precision. To retain the decoded
integer precision for sources up to 32 bits, open the file yourself and decode
packet by packet; the decoder reshapes the buffer to the natural sample type
of the codec (`i16`, MSB-aligned `i32`, or `f32`). The 64-bit integer decoders
keep only the most significant 32 bits.

```rust
use akousma::{AudioBuf, DecodeOptions, DemuxOptions, Hint, Packet};

fn main() -> Result<(), akousma::Error> {
    let file = std::io::BufReader::new(std::fs::File::open("song.wav")?);
    let mut probed = akousma::default_probe().open(
        &Hint::new().with_extension("wav"),
        file,
        &DemuxOptions::new(),
    )?;
    let track = probed.demuxer.default_track().expect("one track").clone();
    let params = track.params;
    let mut decoder = akousma::default_codecs().make(&params, &DecodeOptions::new())?;
    let mut buf = AudioBuf::f32(params.channels, params.sample_rate, params.channel_mask)?;
    let mut packet = Packet::default();
    while probed.demuxer.next_packet(&mut packet)? {
        decoder.decode(&packet, &mut buf)?;
        // A 24- or 32-bit integer source now holds i32 planes:
        if let Some(plane) = buf.plane::<i32>(0) {
            let _exact = plane; // MSB-aligned, no rounding
        }
    }
    decoder.finalize()?;
    Ok(())
}
```

See `crates/akousma/examples/decode_wav.rs` for a complete packet-by-packet
decoder. The source must be seekable. To decode a pipe or socket, first buffer
its contents into a seekable reader such as a `Cursor`.

## Format roadmap

| Area | Formats and codecs | Crate | Status | Release |
| :--- | :--- | :--- | :--- | :--- |
| Core | Byte and bit readers, error and option types, time base, channel layout, audio buffer with gapless trimming, metadata model, format probing, PCM and G.711 decoding | `akousma-core` | Done | 0.1.0 |
| Tags | ID3v1/ID3v2 and `TAG+`, APEv1/APEv2, Lyrics3v2, RIFF INFO | `akousma-meta` | Done | 0.1.0 |
| WAV | RIFF, RIFX, RF64, BW64, Sony Wave64; PCM, IEEE float, G.711 A-law/mu-law, IMA and Microsoft ADPCM | `akousma-wav` | Done | 0.1.0 |
| FLAC | Native FLAC (RFC 9639), Vorbis comments, pictures | `akousma-flac` | Planned | 0.2.0 |
| MP3 | MPEG-1/2/2.5 Layer III with Xing, LAME and VBRI gapless information | `akousma-mp3` | Planned | 0.3.0 |
| MP4 | MP4/M4A (ISO BMFF, fragmented MP4), ADTS; AAC-LC, ALAC | | Planned | later |
| Ogg | Ogg container, Vorbis I | | Planned | later |
| Matroska | MKV/WebM, Opus | | Planned | later |
| Other | MP1/MP2, AIFF, CAF, HE-AAC, MPEG-TS, QOA, WavPack | | Planned | later |

## Minimum supported Rust version

Rust 1.88.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
