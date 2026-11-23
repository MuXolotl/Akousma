# Head-to-head test corpus

The test files are listed by URL and SHA-256 in two manifests. The files are
downloaded for each run, verified, used for testing, and deleted afterward;
they are not committed or distributed by this project.

- `tools/check/corpus.txt` lists 13 files from the FFmpeg FATE suite and the
  Microsoft Multimedia samples. No redistribution licence is known for these
  files, so they are used only for interoperability testing.
- `tools/headtohead/corpus.txt` lists files from the `wavefile` project (MIT),
  the SDL test suite (zlib), and Microsoft Multimedia samples. No
  redistribution licence is known for the Microsoft files; they are used only
  for interoperability testing. Source URLs and SHA-256 hashes were checked
  against upstream files on 2026-11-19 and 2026-11-20.

To run the full 40-file comparison, pass both manifests:

```sh
cargo run --release --manifest-path tools/headtohead/Cargo.toml -- \
  --corpus tools/check/corpus.txt \
  --corpus tools/headtohead/corpus.txt \
  --out COMPARISON.md
```

The runner deduplicates inputs by basename. Avoid basename collisions across
directories and manifests.

## `wavefile/` — rochars/wavefile (MIT)

From the `wavefile` project by Rafael da Silva Rocha
(<https://github.com/rochars/wavefile>), `test/files/` of the master branch
(tree listing via the GitHub API, 2026-11-19). The project's licence is MIT;
these files exercise PCM up to 64-bit float, RIFX (big-endian), RF64, and IMA
ADPCM, including one file whose root size field understates the file.

| File | SHA-256 |
|---|---|
| 27bit-8kHz-noBext-mono.wav | 699a3416f82e020ef649d41af5f19e1248b86fa9d2f73412911629f65c12a999 |
| 4-bit-imaadpcm-8kHz-noBext-mono-reaper.wav | f51b78af5f10570854c717cc1cb52e7b7ca64eb46a888cbbd74eb50ec3008946 |
| 4bit-imaadpcm-8kHz-noBext-mono-encoded.wav | 2195bff2066eabb01a6b4e16ecb570b45d3ed71814ffc38609ffadd4294e0cbd |
| 4bit-imaadpcm-8kHz-noBext-mono.wav | b6f8ecf074c37049e9c452f8cf0ddc70263668c028e515d0a0f552539937c9ce |
| 64bit-48kHz-noBext-mono.wav | 3ad1ed4117bf18656c53252a67515256a22c288da2efcb8cf7cf7d51533f20b5 |
| RF64-16bit-8kHz-stereo-reaper.wav | 000e458550e8bc4bbf87c2408e7a062ffdb1f0952e0dd1c0d2bd081ac411994d |
| RF64-64-bit-8kHz--mono-bext.wav | 4baee3d9aaf88899c01dd2526c9bea1a684c44eccefaeabd57ad880902888857 |
| RIFX-16bit-mono.wav | 70d8d3bae3f5be1d2cb323e9d730abdac9732c517df40e86874a1ef3055f00a4 |
| ima22m.wav | 6f8d19e628aa57ae33939989383b4fdbfa3d5f3f896471e713204c3d3a3337cd |

## `msadpcm-sdl-sample.wav` — SDL test suite (zlib licence)

| File | SHA-256 |
|---|---|
| test/sample.wav | 78788cb7b556fb40049f6e33b70593ac7fda3231fff4b3b9fabe63055bda57c2 |

Microsoft ADPCM, mono, 8 kHz, about 11 seconds. Ships with the SDL test suite
under the zlib licence. Fetched from the GitHub mirror
`libsdl-org/SDL@main:test/sample.wav` (`raw.githubusercontent.com`); the hg
raw-file URL at `hg.libsdl.org` returned HTTP 400 on 2026-11-20, so the pin now
uses the mirror. The bytes are identical — SHA-256 was verified against the
upstream git blob (`002f8158151d600e52930f63b0e832508b6e2c90`) via the GitHub API
on 2026-11-20.

## Microsoft Multimedia samples — `mmsp.ece.mcgill.ca` (no licence grant known)

Source: <https://www.mmsp.ece.mcgill.ca/Documents/AudioFormats/WAVE/>. Used for
interoperability testing only; the same status as the FATE/mmsp list in
`tools/check/corpus.txt`. URLs contain `%20` (escaped spaces) and are used
as-is. "WE" denotes WAVE_FORMAT_EXTENSIBLE. Verified 2026-11-20
(download + Get-FileHash, then deleted).

| File | SHA-256 |
|---|---|
| Samples/AFsp/M1F1-AlawWE-AFsp.wav | c10cf96c55bf6c6d07a0a17172ec42ee2356f168866684f1b46bf0a39881c554 |
| Samples/AFsp/M1F1-mulawWE-AFsp.wav | 80315904994a8889b11552ba9bfc278968fe1d0b10b7e82932148b79175b25ef |
| Samples/AFsp/M1F1-float32-AFsp.wav | 97b727164757c1f6ee556b331cd648fbbde113a2921313c8831907475ccc0477 |
| Samples/AFsp/M1F1-float32WE-AFsp.wav | 75e6b774500487e1bd6b3eb91692873108fa7640f34379a14fb1d1989346a57b |
| Samples/AFsp/M1F1-float64-AFsp.wav | 41108058cb2d5374f04dc85a33aee3132486600504346330b696fb7aac6e3ce2 |
| Samples/AFsp/M1F1-int16WE-AFsp.wav | d1ffc43a0a3eb748b772669bbf492bb7b9c40e5637e38e96752b67f26ee5164b |
| Samples/AFsp/M1F1-int32-AFsp.wav | fe5eaca5210f3bad92b7d155eb29b78b8b6eb1cd62376fab4607d8810567b835 |
| Samples/AFsp/M1F1-int32WE-AFsp.wav | 0faf7b446b97d3252714f1a4179de3f03bf0744b1f397c381b86d10c5a409885 |
| Samples/AFsp/M1F1-uint8WE-AFsp.wav | e160dc4ed220ad3b4296ed8f8ca7ffad60098afa7cc90232a72c20323808bba8 |
| Samples/AFsp/M1F1-int12-AFsp.wav | 3ed8c43b73c71b60c6166d4a8583df865f13870573c417a829530ea70fe13b94 |
| Samples/AFsp/M1F1-int24-AFsp.wav | f6632b210fa2168bf2950ffcf18343c8560e3db815a8ca1cbffc26bbf6776bef |
| Samples/Microsoft/8_Channel_ID.wav | 28d51a19272c1d11445d59028e4500820c7bdc74908c430cb3c6fdf59c9682fd |
| Samples/SoundCardAttrition/4ch.wav | c75be6c9179f9b26e9f9e252d13d04d40b5df680e5d47b950051ab3fb81b243e |
| Samples/Goldwave/addf8-Alaw-GW.wav | beb94fba0333a7e06d1731d07c37574c7231025f0ba73cf9e35b9bad6c3a12aa |
| Samples/Goldwave/addf8-mulaw-GW.wav | 8ae40daa5f3a8c9de8e85e077430dda2ed884e143fa4a5016f704fe29bce8812 |
| Samples/Perverse/Ptjunk.wav | d7eaa5258d9a599e23f46989da4e54b7c7bac48c025330146c3b5b1808f1b1f3 |
| Samples/Perverse/Utopia Critical Stop.WAV | 51e8b50d4919802bebd6e639f75e7a758343b7cdd22db84cbd1f84a9ae0e6a0d |
