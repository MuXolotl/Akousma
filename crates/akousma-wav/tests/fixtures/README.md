# Test fixtures

Files in this directory are synthetic test vectors generated for this
project; they contain no third-party media. They are committed as
byte-for-byte outputs of the commands below, not under a third-party media
licence.

## msadpcm-stereo-22050-ffmpeg.wav

Microsoft ADPCM, stereo, 22 050 Hz, 11 blocks of 1024 bytes (samples per
block 1012, `fact` chunk 11132), generated on 2026-11-20 with the static
FFmpeg build `N-47683-g0e8eb07980` and its native `adpcm_ms` encoder. The
file's `ISFT` field contains `Lavf58.24.100`, that build's libavformat
version; the command below reproduces the SHA-256 byte for byte.

    ffmpeg -nostdin -hide_banner -loglevel error \
      -f lavfi -i 'aevalsrc=exprs=0.5*sin(2*PI*440*t)+0.3*sin(2*PI*2500*t)|0.5*sin(2*PI*554*t)+0.3*sin(2*PI*3200*t):s=22050:d=0.5' \
      -c:a adpcm_ms msadpcm-stereo-22050-ffmpeg.wav

SHA-256: 52e2d91273a4f56b7fefa3ef7233a09bb64edf885bdb903c3d99509bae15716a

`akousma::decode_bytes` decodes it bit-identically to
`ffmpeg -i msadpcm-stereo-22050-ffmpeg.wav -map_metadata -1 -f s16le -`
(verified with `tools/check --dir`, verdict "identical"), so the test pins
both the frame count and the FNV-1a of the decoded planes.
