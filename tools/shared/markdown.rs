//! Text shared by `tools/benchmarks` and `tools/check`, included by both
//! with `#[path]` so that the two reports name inputs the same way.

use akousma::CodecId;

/// `WAV · PCM 16-bit` for the format `wav` and `codec`.
///
/// `CodecId` is `#[non_exhaustive]`: a codec missing here is shown by the
/// `Debug` name of its variant, such as `WAV · PcmS20Le`, which is not a
/// label. Add the label when akousma gets a decoder for it.
pub fn format_label(format: &str, codec: CodecId) -> String {
    use CodecId::*;
    let encoding = match codec {
        PcmU8 => "PCM 8-bit",
        PcmS16Le => "PCM 16-bit",
        PcmS16Be => "PCM 16-bit big-endian",
        PcmS24Le => "PCM 24-bit",
        PcmS24Be => "PCM 24-bit big-endian",
        PcmS32Le => "PCM 32-bit",
        PcmS32Be => "PCM 32-bit big-endian",
        PcmS64Le => "PCM 64-bit",
        PcmS64Be => "PCM 64-bit big-endian",
        PcmF32Le => "Float 32-bit",
        PcmF32Be => "Float 32-bit big-endian",
        PcmF64Le => "Float 64-bit",
        PcmF64Be => "Float 64-bit big-endian",
        G711Alaw => "A-law",
        G711Mulaw => "µ-law",
        AdpcmMs => "MS ADPCM",
        AdpcmImaWav => "IMA ADPCM",
        other => return format!("{} · {other:?}", format.to_uppercase()),
    };
    format!("{} · {encoding}", format.to_uppercase())
}

/// `text` as the content of a Markdown table cell: a `|` would end the cell.
pub fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}
