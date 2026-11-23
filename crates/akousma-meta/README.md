# akousma-meta

Read-only parsers of audio tags for the [Akousma](https://crates.io/crates/akousma)
audio decoding library: ID3v1 with `TAG+`, ID3v2.2 to ID3v2.4 with pictures and
chapters, APEv1 and APEv2, Lyrics3 v2.00 and RIFF `LIST`/`INFO`. It also
scans tags around a file's audio data. Each field keeps its stored key and
value alongside its normalized meaning. It has no dependencies besides
`akousma-core` and no `unsafe` code.

```sh
cargo add akousma-meta
```

To read the tags of a file together with its audio, use
[`akousma`](https://crates.io/crates/akousma).

## License

Licensed under either of the Apache License, Version 2.0
([LICENSE-APACHE](LICENSE-APACHE)) or the MIT license
([LICENSE-MIT](LICENSE-MIT)), at your option.
