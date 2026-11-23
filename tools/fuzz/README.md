# Fuzzing

This standalone [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) package
is excluded from the workspace and is not published. It requires nightly Rust
and a C++ compiler for `libfuzzer-sys`.

## Run a target

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --version 0.13.2 --locked

TARGET=probe # or seek or tags
mkdir -p tools/fuzz/corpus/$TARGET
cp tools/fuzz/seeds/$TARGET/* tools/fuzz/corpus/$TARGET/
cargo +nightly fuzz run $TARGET --fuzz-dir tools/fuzz -- \
  -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

These options run for up to five minutes, treat an input taking over ten
seconds as a timeout, and set a 2048 MB limit for the whole process. The
`Fuzz` GitHub Actions workflow runs all three targets with these limits and
keeps each target's working corpus between runs.

## Targets

- `probe` opens the input with the default probe, decodes the default track,
  and finalizes the decoder if decoding reaches the end.
- `seek` reads one count byte, up to four five-byte seek records, then a media
  file. Each record contains a flags byte and a little-endian `u32`: flag bit 0
  selects accurate seeking; bit 1 selects milliseconds instead of a frame
  number. Frame positions are reduced modulo twice the known track length plus
  one, so some target positions can be past the end. The target reads two
  packets first; after each successful seek, it resets the decoder, reads three
  packets, then reads to the end.
- `tags` uses the first byte to select a parser; the remaining bytes are its
  input. Selector values modulo 7 choose: 0, the tag scanner; 1, ID3v2; 2, APE
  header and items; 3, ID3v1; 4, ID3v1 with `TAG+`; 5, Lyrics3 v2.00; 6, RIFF
  `LIST`/`INFO`. The scanner checks leading ID3v2 and APE tags and trailing
  APE, ID3v1 (including `TAG+`) and Lyrics3 tags; the other parsers read tags
  directly. Inputs shorter than a parser's fixed-size header are skipped.

`probe` and `seek` use the library's default limits, including a 16 MiB packet
limit. The `tags` target calls tag parsers directly, without a media container.

## Seeds and corpus

Seeds for all three targets are committed under `seeds/`:

- `seeds/probe/` contains small WAVE files for PCM (8–32 bit), 32- and 64-bit
  float, A-law, μ-law, IMA and Microsoft ADPCM, WAVE_FORMAT_EXTENSIBLE, RIFX,
  RF64, BW64, Sony Wave64 and `LIST`, plus the cases from
  `crates/akousma-wav/tests/suite/adversarial.rs`.
- `seeds/seek/` contains the WAVE seeds after three seeks: accurate to frame
  4096, coarse to frame 0, and accurate to `2^32 - 1` milliseconds.
- `seeds/tags/` contains valid examples for ID3v2.3, ID3v2.4, APEv2, ID3v1,
  ID3v1 with `TAG+`, Lyrics3 v2.00 and RIFF `INFO`, plus two files for the tag
  scanner: one with ID3v2 before audio and APE, Lyrics3 and ID3v1 after it;
  another with a leading APE tag and a trailing ID3v1 tag.

The working corpus under `corpus/` is created by libFuzzer and is not
committed; copy the seeds for the selected target there before a run.

## Findings

Errors returned while opening, decoding or parsing malformed input are expected.
A finding is a panic, abort, timeout, or process memory use above the configured
limit. libFuzzer writes findings to `artifacts/<target>/` with names such as
`crash-*`, `timeout-*` and `oom-*`.

To turn a finding into a regression test:

1. Reproduce it with `cargo +nightly fuzz run <target> --fuzz-dir tools/fuzz <artifact>`.
2. Minimize it with `cargo +nightly fuzz tmin <target> --fuzz-dir tools/fuzz <artifact>`.
3. Add a regression test for the result required by the format rules; do not
   use the current program's output as the expected result. Put seek cases with
   the seek tests and tag cases beside the relevant parser tests. Add the
   minimized input to `seeds/<target>/` as `case-<name>`.
4. Fix the code and verify that the test passes.
