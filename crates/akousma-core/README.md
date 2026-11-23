# akousma-core

Core building blocks of the [Akousma](https://crates.io/crates/akousma) audio
decoding library. It provides an error type with byte offsets, format limits,
and options; bounded byte and bit readers; time bases and channel layouts; a
planar audio buffer with gapless trimming; codec and demuxer traits; format
probing; metadata types; and PCM and G.711 decoders. It has no dependencies and
no `unsafe` code.

```sh
cargo add akousma-core
```

To decode files, use [`akousma`](https://crates.io/crates/akousma), which
puts this crate together with the format and tag crates.

## License

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license
([LICENSE-MIT](LICENSE-MIT)), at your option.
