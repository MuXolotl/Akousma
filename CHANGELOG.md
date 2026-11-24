# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

---

## [0.1.1] — 2026-11-24

### Changed

- `DemuxOptions` and `DecodeOptions` are `#[non_exhaustive]`, so an option can
  be added without a breaking change. Both are built with `new` and the
  `with_` setters.
- `CodecParams` stays exhaustive on purpose: a decoder may build one to
  describe its output.

### Fixed

- A file whose audio was found no longer depends on `read_metadata` when a
  trailing chunk runs past the end of the source: both modes give the same
  samples, as FFmpeg does. A chunk that does not fit the source before the
  audio is still rejected.

### Performance

- Converting 16-bit stereo planes to `f32` is faster: the stereo path is a
  zipped loop that vectorizes, the generic path stays scalar. The output is
  bit-identical.

---

## [0.1.0] — 2026-11-23

### Added

- The `akousma`, `akousma-core`, `akousma-meta` and `akousma-wav` crates.
- WAVE support for RIFF, RIFX, RF64, BW64 and Sony Wave64, with PCM, IEEE float,
  G.711 A-law and μ-law, IMA ADPCM and Microsoft ADPCM decoding.
- Reading ID3v1, ID3v2, APE, Lyrics3 v2.00 and WAVE metadata, including
  cover art and chapters.
- Packet-based decoding, seeking, gapless trimming and configurable packet, probe and metadata limits.
- Tools for benchmarking, checking decoded output, comparing libraries and fuzzing; timed head-to-head runs rotate the order of in-process libraries to reduce timing bias.
- A release workflow: it sets the version, waits for CI on the resulting commit and then tags, writes the release and publishes the four crates to crates.io in dependency order.
- A generated [`COMPARISON.md`](COMPARISON.md) for a corpus of 40 WAVE files.

### Known limitations

- In a streamed RIFF or RIFX file (`data` size `0xFFFF_FFFF`, or a `data` size
  of 0 in a container with no declared bound) the audio runs to the end of the
  container. Trailing `LIST`, `id3 `, ID3v1, APE or Lyrics3 data after such
  audio is decoded as audio and read as no metadata: the header cannot tell
  those bytes from samples.
- A Wave64 root that ends exactly eight bytes inside the source, as written by
  some muxers that reused RIFF arithmetic, is accepted and the container end is
  raised to the end of the source. Any other root that ends inside the source
  is an invalid header.
- [`decode_file`](crates/akousma/README.md) collects samples as `f32`, so
  32- and 64-bit integer and 64-bit float sources can lose precision. The
  decoders keep at most 32 bits of integer precision.

---

[Unreleased]: https://github.com/MuXolotl/Akousma/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/MuXolotl/Akousma/releases/tag/v0.1.1
[0.1.0]: https://github.com/MuXolotl/Akousma/releases/tag/v0.1.0
