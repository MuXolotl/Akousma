# akousma-wav

WAVE support for the [Akousma](https://crates.io/crates/akousma) audio decoding
library: a demuxer for RIFF, RIFX, RF64, BW64 and Sony Wave64, and decoders
for IMA ADPCM and Microsoft ADPCM. PCM, IEEE float and G.711 are decoded by the
decoder of `akousma-core`. It has no dependencies besides `akousma-core` and
`akousma-meta` and no `unsafe` code.

```sh
cargo add akousma-wav
```

To detect the format and decode a file in one call, use
[`akousma`](https://crates.io/crates/akousma), which enables this crate by
default.

## License

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license
([LICENSE-MIT](LICENSE-MIT)), at your option.
