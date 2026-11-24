# akousma

Akousma is an audio decoding library written in Rust. It reads audio files and
decodes them to sample buffers. It also reads tags, cover art and chapters.
The library has no third-party dependencies, forbids `unsafe` code and does
not write files.

## Supported formats

| Feature | Containers |
| :--- | :--- |
| `wav` (default) | RIFF, RIFX, RF64, BW64, Sony Wave64 |

Audio codecs:

- PCM: unsigned 8-bit; signed 16-, 24-, 32- and 64-bit in both byte orders.
- IEEE float: 32- and 64-bit in both byte orders.
- G.711 A-law and μ-law, IMA ADPCM, Microsoft ADPCM.

The ADPCM decoders follow the specification rather than the fastest published
variant, and the difference is audible in the last bits, not in the formula's
intent:

- IMA ADPCM: the difference is the sum of shifts of the step
  (`step >> shift` plus the set bits of the code), as IMA Recommendation
  Rev 3.00, Appendix D, §6.2 writes it. FFmpeg before 8.1 and Symphonia use the
  multiply form `((2 * code + 1) * step) >> 3`, which can differ by up to two
  least significant bits per sample.
- Microsoft ADPCM: the prediction is divided by 256 truncating toward zero, as
  in the Multimedia Data Standards Update (1994); an arithmetic shift by 8
  would round negative predictions down.

Neither choice changes the decoded length, only the last bits of each sample.

Tags around the audio data (ID3v2 and APE at the start; APE, ID3v1, `TAG+` and
Lyrics3 v2.00 at the end) are read for every format. Malformed tags are ignored. If
the source fails or ends early while a tag is being read, opening the file
returns that source error. WAVE files can also contain `LIST`/`INFO` fields,
embedded ID3v2 tags, the broadcast extension (`bext`), sampler and loop chunks,
and cue points used as chapters.

## Decoding a whole file

[`decode_file`] and [`decode_bytes`] detect the format, decode the default
track and return interleaved `f32` samples with the metadata:

```no_run
use akousma::Error;

fn main() -> Result<(), Error> {
    let decoded = akousma::decode_file("song.wav")?;
    println!(
        "{}: {} Hz, {} channels, {} frames",
        decoded.format,
        decoded.params.sample_rate,
        decoded.params.channels,
        decoded.frames(),
    );
    Ok(())
}
```

A container whose declared length runs past the end of the source is
truncated, not an error: the audio that is present decodes. Compare
`frames()` with `params.total_frames` to notice.

## Streaming

For long files, read packets with a [`Demuxer`] and decode them into a
reused [`AudioBuf`]. Reusing the buffer avoids repeated allocations while
decoding.

The source must be seekable. To decode a pipe or socket, first buffer its
contents into a seekable reader such as a `Cursor`.

```no_run
use std::fs::File;
use std::io::BufReader;

use akousma::{AudioBuf, ChannelMask, DecodeOptions, DemuxOptions, Hint, Packet};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let probe = akousma::default_probe();
    let codecs = akousma::default_codecs();
    let hint = Hint::new().with_extension("wav");
    let file = BufReader::new(File::open("song.wav")?);
    let mut probed = probe.open(&hint, file, &DemuxOptions::new())?;
    let track = probed.demuxer.default_track().expect("one track").clone();
    let mut decoder = codecs.make(&track.params, &DecodeOptions::new())?;

    // The decoder reshapes the buffer to the stream on the first packet.
    let mut buf = AudioBuf::f32(1, track.params.sample_rate, ChannelMask::EMPTY)?;
    let mut packet = Packet::default();
    let mut interleaved = Vec::new();
    while probed.demuxer.next_packet(&mut packet)? {
        decoder.decode(&packet, &mut buf)?;
        interleaved.resize(buf.samples(), 0.0);
        buf.copy_interleaved_f32(&mut interleaved);
        // `interleaved` holds the samples of this packet.
    }
    println!("{} packets of {}", probed.format, track.params.sample_rate);
    Ok(())
}
```

Decoded samples use `i16` planes for 16-bit and narrower integer sources,
`i32` planes for 24- and 32-bit integer sources (significant bits at the top),
and `f32` planes for float sources. A 64-bit integer source also uses `i32`
planes, retaining its most significant 32 bits. [`AudioBuf::copy_interleaved_f32`]
and [`AudioBuf::copy_interleaved_i16`] convert any of these planes.

Channels are in the order of the WAVEFORMATEXTENSIBLE speaker bits (front
left, front right, front center, LFE, ...); [`ChannelMask`] names them.

## Seeking and gapless playback

[`Demuxer::seek`] takes a frame or a time. With [`SeekMode::Accurate`] the
next decoded buffer starts exactly at the target; demuxers that can only land
on a block or a key frame mark the frames to drop in
[`Packet::trim_start`]. Encoder delay and padding are marked the same way in
[`Packet::trim_start`] and [`Packet::trim_end`] when
[`DemuxOptions::gapless`] is set (the default), and decoders apply them when
[`DecodeOptions::apply_trim`] is set (the default).

## Metadata

[`Demuxer::metadata`] returns tag revisions in file order: leading tags,
container metadata, then trailing tags. Conflicting values are all kept; the
application decides which one to use. Each [`meta::Tag`] includes the stored
key and value and, when available, a normalized [`meta::StandardTag`]. Gain
values are metadata only; the decoder does not apply them.

The title, the front cover and the chapters of a decoded file:

```no_run
use akousma::meta;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let decoded = akousma::decode_file("song.wav")?;
    for revision in &decoded.metadata {
        for tag in &revision.tags {
            if let Some(meta::StandardTag::TrackTitle(title)) = &tag.standard {
                println!("title: {title}");
            }
        }
        for visual in &revision.visuals {
            if visual.kind == meta::VisualKind::FrontCover {
                std::fs::write("cover.jpg", &visual.data)?;
            }
        }
    }
    for chapter in &decoded.chapters {
        println!("{} at {:?}", chapter.id, chapter.start);
    }
    Ok(())
}
```

## Limits

Akousma checks declared input sizes against configured limits before
reserving memory. These limits constrain input bytes and entry counts; they
are not exact limits on total memory use, because parsed values and container
structures also use memory.

| Limit | Default | What it limits |
| :--- | :--- | :--- |
| [`FormatLimits::max_packet_bytes`] | 16 MiB | Bytes in one audio packet or compressed block. The demuxer fails if even one frame or block does not fit. |
| [`FormatLimits::max_metadata_bytes`] | 32 MiB | Total input bytes of metadata read across the file. A metadata block that does not fit in the remaining budget fails with [`ErrorKind::LimitExceeded`]; it is not skipped. |
| [`FormatLimits::max_tag_scan_bytes`] | 32 MiB | Bytes scanned for APEv2 and Lyrics3 v2.00 tags when `read_metadata` is disabled. Exhausting this limit fails instead of leaving an unchecked tag in the audio. |
| [`FormatLimits::max_tracks`] | 64 | Number of tracks accepted from a container. |
| [`FormatLimits::max_probe_bytes`] | 1 MiB | Bytes read while detecting the format and container length. Bytes skipped by seeking do not count. Too small a limit, including 0, can prevent opening. |

Metadata also has a separate entry limit of
[`meta::MetadataBudget::MAX_ENTRIES`] for tags, pictures, chapters, cue points
and labels. The byte limits bound input, not the memory of the parsed result.
When `read_metadata` is disabled, the scanner still checks APEv2 and Lyrics3
v2.00 tags that determine the audio boundary, using a fixed-size buffer and
skipping picture and other binary payloads. This preserves the same audio
range as reading metadata, provided `max_tag_scan_bytes` is at least
`max_metadata_bytes`. Other metadata blocks need not be read; a container
demuxer may stop scanning once it reaches the audio data.

### WAVE file boundaries

- RIFF and RIFX root sizes of 0 or `0xFFFF_FFFF` declare no bound. RF64 and
  BW64 use the size from `ds64` when the root size is `0xFFFF_FFFF`; a root
  size or resolved `ds64` size of 0 also declares no bound. Wave64 uses its
  64-bit root size.
- A chunk that extends past the declared root end is accepted if it is fully
  present in the source, and scanning then continues to the source end. An
  incomplete chunk is an invalid header. Wave64 also accepts a root size
  exactly eight bytes short of the source length, as written by some muxers.
- In RIFF and RIFX, a `data` size of `0xFFFF_FFFF` means audio continues to
  the container end. A `data` size of 0 means the same only when the
  container has no declared bound; otherwise the `data` chunk is empty. Bytes
  after audio that runs to the end are treated as audio because the header
  cannot distinguish them from audio data, so trailing tags of such a file
  are decoded as audio and not read as metadata.
- A `data` chunk whose declared size runs past both the root end and the end
  of the source is an invalid header: the header cannot say how much audio is
  missing, so the file is rejected rather than cut to an arbitrary length. A
  `data` chunk that fits the source but not the declared root is read to the
  end of the source, as above.
- Metadata chunks after the audio are read only when `read_metadata` is
  enabled. Without it, they are not checked by the WAVE demuxer. A trailing
  chunk whose declared size runs past the end of the source ends the scan
  instead of failing the file: the audio is already found and its length is
  settled, so the same samples come out with or without `read_metadata`. A
  chunk that does not fit the source *before* the audio is still an invalid
  header, because there the header cannot say how much audio is missing.
- A `WAVEFORMATEXTENSIBLE` `wValidBitsPerSample` below the container size is
  reported but does not move the samples: 20 valid bits in a 24-bit container
  decode as 24-bit words in the most significant bits of the `i32` plane, the
  same values FFmpeg returns. The Microsoft text places the valid bits in the
  most significant part of the container, so the samples need no shift; a file
  that stores them in the least significant bits instead decodes quietly and
  looks like a header that lied, and the header alone cannot tell the two
  apart.
- `LIST wavl`, the RIFF form that lists alternating `data` and `slnt` chunks,
  is not read: the file is rejected with an invalid header. The RIFF
  specification allows the form; writable audio that reaches this decoder in
  practice keeps a plain `data` chunk. FFmpeg has no `wavl` support either.

`block_align` of 0 is rejected with an invalid header, as are a sample rate of
0 and more than 64 channels. FFmpeg accepts all three by guessing frame sizes;
this decoder does not guess, because a wrong guess silently shifts every sample
after it.

## Crates

This crate re-exports the others. They can also be used on their own:

| Crate | Content |
| :--- | :--- |
| `akousma-core` | buffers, codec and container traits, readers, checksums, transforms, PCM decoder |
| `akousma-meta` | tag parsers ([`tags`]) |
| `akousma-wav` | WAVE demuxer and ADPCM decoders (`wav`) |

## Publishing

For maintainers. `akousma-meta`, `akousma-wav` and `akousma` depend on
`akousma-core` (and on each other) by version, so `cargo publish` cannot
publish the workspace in one command: until a dependency is on crates.io, the
crates that need it fail with ``no matching package named `akousma-core`
found``, and `--no-verify` does not help. Publish them in this order, waiting
after each one until crates.io has indexed it:

1. `akousma-core`
2. `akousma-meta`
3. `akousma-wav`
4. `akousma`

The crates require each other as `0.1.0`, which accepts any 0.1.x, so a 0.1.x
fix of one crate reaches users through `cargo update`. To make the fixed
version the minimum, raise the requirement in every crate after it in this
list and publish the whole chain again, in the same order.
