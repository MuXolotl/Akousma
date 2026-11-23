//! Compares the output of akousma with the ffmpeg CLI, sample by sample, on
//! files that akousma did not write.
//!
//! ```text
//! cargo run --release -- (--dir PATH | --corpus FILE) [--ffmpeg PATH]
//! ```
//!
//! Prints a Markdown table with one row per file. The exit code is 0 when
//! every file passes, 1 when a file fails and 2 when the check itself cannot
//! run (bad arguments, a failed download, a wrong SHA-256).
#![forbid(unsafe_code)]

#[path = "../../shared/markdown.rs"]
mod markdown;
mod sha256;

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use akousma::{CodecId, DemuxOptions, ErrorKind, Hint};

use markdown::{cell, format_label};

const USAGE: &str = "usage: akousma-check (--dir PATH | --corpus FILE) [--ffmpeg PATH]";

/// Peak deviation from ffmpeg above which an ADPCM output counts as broken
/// rather than rounded differently: about 330 LSB of 16-bit audio. A broken
/// decoder deviates by 0.1 to 1.0; the rounding difference depends on the
/// signal and the length, so it is shown but never judged.
const ADPCM_BOUND: f32 = 1e-2;

type Res<T> = Result<T, String>;

enum Source {
    Dir(PathBuf),
    Corpus(PathBuf),
}

struct Args {
    source: Source,
    ffmpeg: PathBuf,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Res<Args> {
    let (mut source, mut ffmpeg) = (None, PathBuf::from("ffmpeg"));
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value\n{USAGE}"));
        let next = match flag.as_str() {
            "--dir" => Source::Dir(value()?.into()),
            "--corpus" => Source::Corpus(value()?.into()),
            "--ffmpeg" => {
                ffmpeg = value()?.into();
                continue;
            }
            _ => return Err(format!("unknown argument {flag}\n{USAGE}")),
        };
        if source.replace(next).is_some() {
            return Err(format!("give either --dir or --corpus, once\n{USAGE}"));
        }
    }
    let source = source.ok_or(USAGE)?;
    Ok(Args { source, ffmpeg })
}

/// How the output of a codec is judged against ffmpeg.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    /// Every sample bit for bit.
    Exact,
    /// The frame count only; the peak deviation is shown, with its reason
    /// when it is known, and fails only above [`ADPCM_BOUND`].
    Adpcm(Option<&'static str>),
}

fn rule(codec: CodecId) -> Rule {
    match codec {
        // akousma uses the shift-add form of the IMA reference decoder; ffmpeg
        // used to multiply (8.0.x still does, 9.0.x matches bit for bit), so the
        // two may differ by design. The deviation is shown but never judged.
        CodecId::AdpcmImaWav => Rule::Adpcm(Some("shift-add vs multiply")),
        // Not made exact although the samples run matched ffmpeg bit for bit:
        // the ffmpeg encoder always writes predictor 0, the one case where
        // truncation and a shift agree, so a zero bound would pass with either
        // rounding. The rounding of predictors 3, 4 and 6 is tested in the
        // crate.
        CodecId::AdpcmMs => Rule::Adpcm(None),
        _ => Rule::Exact,
    }
}

/// A position in interleaved samples with the two values there.
fn at(index: usize, channels: usize, ours: f32, theirs: f32) -> String {
    let (frame, channel) = (index / channels, index % channels);
    format!("frame {frame}, channel {channel}: akousma {ours}, ffmpeg {theirs}")
}

/// The part of the ffmpeg output that corresponds to akousma's, and a note
/// when they differ by the `padding` frames that akousma trims.
///
/// A `fact` chunk gives the true frame count of an ADPCM stream, whose last
/// block is padded; akousma ends the stream there and reports the trimmed
/// frames as `padding`, while ffmpeg decodes every block whole. Only an
/// excess of exactly `padding` frames is taken for that; any other count
/// stays a mismatch.
fn untrimmed(ours: usize, theirs: &[f32], padding: usize, channels: usize) -> (&[f32], String) {
    match padding > 0 && theirs.len() == ours + padding * channels {
        true => (
            &theirs[..ours],
            format!(" (ffmpeg keeps the {padding} frames that fact trims)"),
        ),
        false => (theirs, String::new()),
    }
}

/// The verdict on two interleaved outputs and whether it is a failure.
fn compare(ours: &[f32], theirs: &[f32], channels: usize, rule: Rule) -> (String, bool) {
    if ours.len() != theirs.len() {
        let (a, b) = (ours.len() / channels, theirs.len() / channels);
        return (format!("akousma {a} frames, ffmpeg {b} frames"), true);
    }
    let pairs = || ours.iter().zip(theirs).enumerate();
    let Some((first, (a, b))) = pairs().find(|(_, (a, b))| a.to_bits() != b.to_bits()) else {
        return ("identical".into(), false);
    };
    let Rule::Adpcm(reason) = rule else {
        return (format!("differs at {}", at(first, channels, *a, *b)), true);
    };
    let peak = pairs().map(|(_, (a, b))| (a - b).abs()).fold(0.0, f32::max);
    let shown = match reason {
        Some(reason) => format!("{reason}, max abs diff {peak:.1e}"),
        None => format!("max abs diff {peak:.1e}"),
    };
    match pairs().find(|(_, (a, b))| (*a - *b).abs() > ADPCM_BOUND) {
        None => (shown, false),
        Some((i, (a, b))) => {
            let bound = format!(
                "above {ADPCM_BOUND:.0e} first at {}",
                at(i, channels, *a, *b)
            );
            (format!("{shown}, {bound}"), true)
        }
    }
}

/// The interleaved samples that ffmpeg decodes from `path`, the same way as
/// the benchmark runs it.
fn ffmpeg(program: &Path, path: &Path) -> Res<Vec<f32>> {
    let mut command = Command::new(program);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    command.args(["-threads", "1", "-i"]).arg(path);
    command.args(["-f", "f32le", "-"]).stdin(Stdio::null());
    let output = command.output();
    let output = output.map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.lines().next().unwrap_or("no message").to_string());
    }
    let (samples, rest) = output.stdout.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(format!("{} trailing bytes in the f32 output", rest.len()));
    }
    Ok(samples.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

struct Row {
    file: String,
    format: String,
    duration: String,
    verdict: String,
    failed: bool,
}

/// The container and the codec that akousma reports for `bytes`, if it opens
/// them.
fn label(bytes: &[u8]) -> Option<String> {
    let source = Cursor::new(bytes.to_vec());
    let probed = akousma::default_probe().open(&Hint::new(), source, &DemuxOptions::new());
    let probed = probed.ok()?;
    let codec = probed.demuxer.default_track()?.params.codec;
    Some(format_label(probed.format, codec))
}

fn check(path: &Path, program: &Path) -> Res<Row> {
    let name = path.file_name().unwrap_or(path.as_os_str());
    let file = name.to_string_lossy().into_owned();
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let format = label(&bytes).unwrap_or_else(|| "—".into());
    let row = |duration: String, verdict: String, failed| Row {
        file: file.clone(),
        format: format.clone(),
        duration,
        verdict,
        failed,
    };
    let decoded = match akousma::decode_bytes(bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            use ErrorKind::{UnsupportedCodec, UnsupportedFeature, UnsupportedFormat};
            let outside = matches!(
                error.kind,
                UnsupportedFormat | UnsupportedCodec | UnsupportedFeature
            );
            let verdict = match outside {
                true => format!("rejected: {error}"),
                false => format!("decode fails: {error}"),
            };
            return Ok(row("—".into(), verdict, !outside));
        }
    };
    let params = &decoded.params;
    let seconds = decoded.frames() as f64 / f64::from(params.sample_rate);
    let duration = format!("{seconds:.1} s");
    let (verdict, failed) = match ffmpeg(program, path) {
        Ok(theirs) => {
            let (ours, channels) = (&decoded.samples, usize::from(params.channels));
            let padding = usize::try_from(params.padding).map_err(|e| e.to_string())?;
            let (theirs, note) = untrimmed(ours.len(), &theirs, padding, channels);
            let (verdict, failed) = compare(ours, theirs, channels, rule(params.codec));
            (verdict + &note, failed)
        }
        Err(error) => (format!("ffmpeg fails: {error}"), true),
    };
    Ok(row(duration, verdict, failed))
}

fn table(rows: &[Row]) -> String {
    let mut out = String::from("| File | Format | Duration | Verdict |\n|---|---|---:|---|\n");
    for row in rows {
        let cells = [&row.file, &row.format, &row.duration, &row.verdict];
        let cells: Vec<_> = cells.iter().map(|text| cell(text)).collect();
        out += &format!("| {} |\n", cells.join(" | "));
    }
    out
}

/// The files of `dir`, by name.
fn listed(dir: &Path) -> Res<Vec<PathBuf>> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    match files.is_empty() {
        true => Err(format!("no files in {}", dir.display())),
        false => Ok(files),
    }
}

/// The `URL SHA-256` pairs of a corpus list; blank lines and `#` comments
/// are skipped.
fn corpus_entries(list: &str) -> Res<Vec<(&str, &str)>> {
    let mut entries = Vec::new();
    for (number, line) in list.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        let [url, digest] = fields[..] else {
            return Err(format!("line {}: expected `URL SHA-256`", number + 1));
        };
        let hex = digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !hex {
            return Err(format!(
                "line {}: the SHA-256 is not 64 lowercase hex digits",
                number + 1
            ));
        }
        entries.push((url, digest));
    }
    match entries.is_empty() {
        true => Err("the corpus list is empty".into()),
        false => Ok(entries),
    }
}

/// Downloads every file of the corpus `list` into `dir` with curl and
/// verifies its SHA-256; the downloaded paths in the order of the list.
fn fetch(list: &Path, dir: &Path) -> Res<Vec<PathBuf>> {
    let text = fs::read_to_string(list);
    let text = text.map_err(|e| format!("cannot read {}: {e}", list.display()))?;
    let mut files = Vec::new();
    for (url, digest) in corpus_entries(&text)? {
        let name = url.rsplit('/').next().filter(|name| !name.is_empty());
        let path = dir.join(name.ok_or(format!("{url}: no file name"))?);
        if files.contains(&path) {
            return Err(format!("{url}: a second file with this name"));
        }
        let status = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--retry",
                "3",
            ])
            .arg("--output")
            .arg(&path)
            .arg(url)
            .stdin(Stdio::null())
            .status();
        let status = status.map_err(|e| format!("cannot run curl: {e}"))?;
        if !status.success() {
            return Err(format!("{url}: curl failed ({status})"));
        }
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let actual = sha256::hex(&bytes);
        if actual != digest {
            return Err(format!("{url}: SHA-256 {actual}, expected {digest}"));
        }
        files.push(path);
    }
    Ok(files)
}

fn report(files: &[PathBuf], program: &Path) -> Res<bool> {
    let rows = files.iter().map(|path| check(path, program));
    let rows = rows.collect::<Res<Vec<_>>>()?;
    print!("{}", table(&rows));
    Ok(rows.iter().all(|row| !row.failed))
}

fn run() -> Res<bool> {
    let args = parse_args(std::env::args().skip(1))?;
    match &args.source {
        Source::Dir(dir) => report(&listed(dir)?, &args.ffmpeg),
        Source::Corpus(list) => {
            let dir = std::env::temp_dir().join(format!("akousma-check-{}", std::process::id()));
            fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let result = fetch(list, &dir).and_then(|files| report(&files, &args.ffmpeg));
            let removed = fs::remove_dir_all(&dir);
            let removed = removed.map_err(|e| format!("cannot remove {}: {e}", dir.display()));
            result.and_then(|passed| removed.map(|()| passed))
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("akousma-check: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMA: Rule = Rule::Adpcm(Some("shift-add vs multiply"));

    #[test]
    fn exact_outputs_must_match_bit_for_bit() {
        let ours = [0.5, -0.25, 0.0, 1.0];
        assert_eq!(
            compare(&ours, &ours, 2, Rule::Exact),
            ("identical".into(), false)
        );
        // -0.0 and 0.0 compare equal as floats but are different outputs.
        let theirs = [0.5, -0.25, -0.0, 1.0];
        let expected = "differs at frame 1, channel 0: akousma 0, ffmpeg -0";
        assert_eq!(
            compare(&ours, &theirs, 2, Rule::Exact),
            (expected.into(), true)
        );
    }

    #[test]
    fn a_frame_count_difference_always_fails() {
        let verdict = ("akousma 2 frames, ffmpeg 1 frames".into(), true);
        assert_eq!(compare(&[0.0; 4], &[0.0; 2], 2, Rule::Exact), verdict);
        assert_eq!(compare(&[0.0; 4], &[0.0; 2], 2, IMA), verdict);
    }

    #[test]
    fn adpcm_deviation_is_shown_and_fails_only_above_the_bound() {
        let ours = [0.1, 0.2, 0.3];
        let near = [0.1, 0.2026, 0.3];
        let expected = "shift-add vs multiply, max abs diff 2.6e-3";
        assert_eq!(compare(&ours, &near, 1, IMA), (expected.into(), false));
        let (verdict, failed) = compare(&ours, &[0.1, 0.2, 0.5], 1, Rule::Adpcm(None));
        let expected =
            "max abs diff 2.0e-1, above 1e-2 first at frame 2, channel 0: akousma 0.3, ffmpeg 0.5";
        assert_eq!((verdict.as_str(), failed), (expected, true));
    }

    #[test]
    fn only_the_padding_that_fact_trims_is_ignored() {
        let (ours, theirs) = ([0.1, 0.2, 0.3, 0.4], [0.1, 0.2, 0.3, 0.4, 0.0, 0.0]);
        let note = " (ffmpeg keeps the 1 frames that fact trims)";
        assert_eq!(untrimmed(4, &theirs, 1, 2), (&theirs[..4], note.into()));
        // One padding frame declared, two extra frames decoded: a mismatch.
        assert_eq!(
            untrimmed(2, &theirs[..4], 1, 1),
            (&theirs[..4], String::new())
        );
        assert_eq!(untrimmed(4, &ours, 0, 2), (&ours[..], String::new()));
        let (cut, _) = untrimmed(4, &theirs, 1, 2);
        assert_eq!(compare(&ours, cut, 2, IMA), ("identical".into(), false));
    }

    #[test]
    fn rules_by_codec() {
        assert_eq!(rule(CodecId::PcmS16Le), Rule::Exact);
        assert_eq!(rule(CodecId::G711Mulaw), Rule::Exact);
        assert_eq!(rule(CodecId::AdpcmImaWav), IMA);
        assert_eq!(rule(CodecId::AdpcmMs), Rule::Adpcm(None));
    }

    #[test]
    fn corpus_lists() {
        let digest = "3bbb7f3578d61f2df514b40667a930d00efd1ab9ec11eaef608d0ab0dd586a17";
        let list = format!("# comment\n\nhttps://example.org/a.wav  {digest}\n");
        let entries = corpus_entries(&list).unwrap();
        assert_eq!(entries, [("https://example.org/a.wav", digest)]);
        let upper = format!("https://example.org/a.wav {}", digest.to_uppercase());
        assert!(corpus_entries(&upper).is_err());
        assert!(corpus_entries("https://example.org/a.wav").is_err());
        assert!(corpus_entries("# nothing\n").is_err());
    }

    #[test]
    fn arguments() {
        let args = |list: &[&str]| parse_args(list.iter().map(|s| s.to_string()));
        let parsed = args(&["--corpus", "c.txt", "--ffmpeg", "/opt/ffmpeg"]).unwrap();
        assert!(matches!(parsed.source, Source::Corpus(ref p) if p == Path::new("c.txt")));
        assert_eq!(parsed.ffmpeg, Path::new("/opt/ffmpeg"));
        assert!(args(&["--dir", "a", "--corpus", "b"]).is_err());
        assert!(args(&["--ffmpeg", "ffmpeg"]).is_err());
        assert!(args(&["--dir"]).is_err());
    }

    #[test]
    fn pipes_in_cells_are_escaped() {
        let row = Row {
            file: "a|b.wav".into(),
            format: "WAV · PCM 16-bit".into(),
            duration: "0.5 s".into(),
            verdict: "identical".into(),
            failed: false,
        };
        let table = table(&[row]);
        assert!(table.ends_with("| a\\|b.wav | WAV · PCM 16-bit | 0.5 s | identical |\n"));
    }
}
