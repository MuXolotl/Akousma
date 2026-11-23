//! akousma against symphonia, hound and the ffmpeg CLI on a corpus of WAVE files.
//!
//! ```text
//! cargo run --release -- [--corpus tools/headtohead/corpus.txt]... [--dir PATH]...
//!     [--ffmpeg PATH] [--runs N] [--out PATH]
//! ```
//!
//! The tool recursively collects `.wav` and `.w64` files from the given
//! directories and files from the supplied corpus manifests. Inputs with the
//! same basename are deduplicated. It compares akousma, symphonia and `hound`
//! in process. One untimed decode captures output for
//! comparison; timed decodes are interleaved, and the starting library rotates
//! between iterations to reduce bias from execution order. The median time is
//! reported. The `ffmpeg` CLI is timed end to end on a temporary file, including
//! process startup, file reading, decoding and output. Those measurements are
//! not directly comparable with the in-process timings. `ffprobe` provides the
//! channel count for ffmpeg's raw `f32le` output.
//!
//! The Markdown report lists the machine, coverage, per-file results, decoding
//! times, output differences and rejections. It is generated and written to
//! `--out` (stdout by default); do not edit it by hand.
//!
//! The report is also the comparison document in `COMPARISON.md`. Corpus files
//! are pinned by URL and SHA-256 in `tools/check/corpus.txt` (13 files) and
//! `tools/headtohead/corpus.txt` (27 files). They are downloaded to a temporary
//! directory, verified, used for the run and deleted afterward. Pass both
//! manifests to use the full 40-file set. Results depend on the machine and
//! corpus and should not be compared directly across different setups.

#![forbid(unsafe_code)]

use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;

#[path = "../../check/src/sha256.rs"]
mod sha256;

const USAGE: &str =
    "usage: akousma-headtohead [--dir PATH]... [--corpus FILE]... [--ffmpeg PATH]
    [--runs N] [--out PATH]";

/// One decoded file.
struct Decode {
    frames: u64,
    ns_per_frame: f64,
    samples: Vec<f32>,
}

/// Everything one library did with one file.
struct FileReport {
    name: String,
    akousma: Result<Decode, String>,
    symphonia: Result<Decode, String>,
    hound: Result<Decode, String>,
    /// `None` when `ffmpeg` is not available on this machine.
    ffmpeg: Option<Result<Decode, String>>,
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("akousma-headtohead: cannot read {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => collect(&path, files),
            Ok(_) => match path.extension().and_then(|e| e.to_str()) {
                Some(ext) if matches!(ext.to_ascii_lowercase().as_str(), "wav" | "w64") => {
                    files.push(path)
                }
                _ => {}
            },
            Err(_) => {}
        }
    }
}

fn akousma_once(bytes: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let decoded = akousma::decode_bytes(bytes.to_vec()).map_err(|e| e.to_string())?;
    Ok((u32::from(decoded.params.channels), decoded.samples))
}

fn symphonia_once(bytes: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let stream = symphonia::core::io::MediaSourceStream::new(
        Box::new(Cursor::new(bytes.to_vec())),
        Default::default(),
    );
    let probe = symphonia::default::get_probe();
    let mut format = probe
        .probe(&Hint::new(), stream, Default::default(), Default::default())
        .map_err(|e| e.to_string())?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "no audio track".to_string())?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "not audio".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut channels = None;
    while let Some(packet) = format.next_packet().map_err(|e| e.to_string())? {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|e| e.to_string())?;
        if channels.is_none() {
            channels = Some(decoded.spec().channels().count() as u32);
        }
        let needed = decoded.samples_interleaved();
        let start = out.len();
        out.resize(start + needed, 0.0);
        decoded.copy_to_slice_interleaved::<f32, _>(&mut out[start..]);
    }
    let channels = channels.ok_or_else(|| "nothing decoded".to_string())?;
    Ok((channels, out))
}

fn hound_once(bytes: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut reader = hound::WavReader::new(cursor).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err("hound: zero channels".into());
    }
    let (format, bits) = (spec.sample_format, spec.bits_per_sample);
    let channels = u32::from(spec.channels);
    if bits == 0 || bits > 32 {
        return Err(format!("hound: unsupported sample width {bits}"));
    }
    let scale = 1.0 / (1u64 << (bits - 1)) as f32;
    let mut out = Vec::new();
    match (format, bits) {
        (hound::SampleFormat::Float, 32) => {
            for sample in reader.samples::<f32>() {
                out.push(sample.map_err(|e| e.to_string())?);
            }
        }
        (hound::SampleFormat::Float, _) => {
            return Err(format!("hound: float samples of {bits} bits are not supported"));
        }
        (hound::SampleFormat::Int, ..=16) => {
            for sample in reader.samples::<i16>() {
                out.push(f32::from(sample.map_err(|e| e.to_string())?) * scale);
            }
        }
        (hound::SampleFormat::Int, _) => {
            for sample in reader.samples::<i32>() {
                out.push(sample.map_err(|e| e.to_string())? as f32 * scale);
            }
        }
    }
    Ok((channels, out))
}

/// The per-library result of one warm-up decode.
struct WarmUp {
    frames: u64,
    samples: Vec<f32>,
}

/// One untimed decode per library (kept for the sample comparison), then
/// `runs` timed decodes of each, interleaved: akousma, symphonia and hound are
/// timed one after another inside every iteration, and the library that starts
/// rotates each iteration, so frequency scaling, cache state, core migration
/// and heap-state noise land on all three equally instead of favouring
/// whichever library ran in a quieter moment or at a luckier position.
/// Every failure is independent: one library rejecting the file does not
/// affect the others. The median ns per frame is reported.
fn time_runs_interleaved(
    once: [fn(&[u8]) -> Result<(u32, Vec<f32>), String>; 3],
    bytes: &[u8],
    runs: usize,
) -> [Result<Decode, String>; 3] {
    let mut states: [Result<WarmUp, String>; 3] = [
        Err("not run".to_string()),
        Err("not run".to_string()),
        Err("not run".to_string()),
    ];
    for (i, library) in once.iter().enumerate() {
        states[i] = match library(bytes) {
            Ok((channels, samples)) => {
                let frames = samples.len() as u64 / u64::from(channels);
                match frames {
                    0 => Err("no frames decoded".to_string()),
                    frames => Ok(WarmUp { frames, samples }),
                }
            }
            Err(error) => Err(error),
        };
    }
    let mut timed: [Vec<f64>; 3] = [
        Vec::with_capacity(runs),
        Vec::with_capacity(runs),
        Vec::with_capacity(runs),
    ];
    for iteration in 0..runs {
        // Rotate the starting library each iteration, so no library is stuck
        // with the heap state the previous library left behind.
        for offset in 0..3 {
            let i = (iteration + offset) % 3;
            if states[i].is_err() {
                continue;
            }
            let start = Instant::now();
            let decoded = once[i](bytes);
            let elapsed = start.elapsed();
            match decoded {
                Ok((channels, samples)) => {
                    black_box(&samples);
                    let frames = samples.len() as u64 / u64::from(channels);
                    timed[i].push(elapsed.as_nanos() as f64 / frames as f64);
                }
                Err(error) => states[i] = Err(error),
            }
        }
    }
    let mut results: [Result<Decode, String>; 3] = [
        Err("not run".to_string()),
        Err("not run".to_string()),
        Err("not run".to_string()),
    ];
    for (i, state) in states.into_iter().enumerate() {
        results[i] = match state {
            Ok(WarmUp { frames, samples }) => {
                let mut ns = std::mem::take(&mut timed[i]);
                ns.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs in timings"));
                Ok(Decode {
                    frames,
                    ns_per_frame: ns[ns.len() / 2],
                    samples,
                })
            }
            Err(error) => Err(error),
        };
    }
    results
}

/// Executable names to try for `ffmpeg` and friends, per platform.
fn exe_names(name: &str) -> Vec<String> {
    match cfg!(windows) {
        true => vec![format!("{name}.exe"), name.to_string()],
        false => vec![name.to_string()],
    }
}

fn search_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .flat_map(|dir| exe_names(name).into_iter().map(move |exe| dir.join(exe)))
        .find(|candidate| candidate.is_file())
}

fn first_line(program: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}

/// The `ffmpeg` command line, located once for the whole run.
struct Ffmpeg {
    program: PathBuf,
    ffprobe: PathBuf,
    version: String,
}

impl Ffmpeg {
    /// Find `ffmpeg` (and `ffprobe` beside it or in `PATH`); `None` when not found.
    fn find(explicit: Option<PathBuf>) -> Option<Ffmpeg> {
        let program = explicit.or_else(|| search_path("ffmpeg"))?;
        let beside = program
            .parent()
            .map(|dir| dir.join(if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" }))
            .filter(|candidate| candidate.is_file());
        let ffprobe = beside.or_else(|| search_path("ffprobe"))?;
        let version = first_line(&program, &["-version"])
            .map(|line| line.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|| "ffmpeg (version unknown)".to_string());
        Some(Ffmpeg {
            program,
            ffprobe,
            version,
        })
    }

    /// The channel count of the first audio stream, from `ffprobe`.
    fn channels(&self, file: &Path) -> Result<u32, String> {
        let output = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=channels",
                "-of",
                "csv=p=0",
            ])
            .arg(file)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.ffprobe.display()))?;
        if !output.status.success() {
            return Err(format!("ffprobe exited with {}", output.status));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let text = text.trim();
        text.parse::<u32>()
            .ok()
            .filter(|channels| *channels > 0)
            .ok_or_else(|| format!("ffprobe: unexpected output `{text}`"))
    }

    /// Decode one file end to end; `channels` comes from `ffprobe`.
    fn decode(&self, file: &Path, channels: u32) -> Result<(u32, Vec<f32>), String> {
        let output = Command::new(&self.program)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                "1",
                "-i",
            ])
            .arg(file)
            .args(["-f", "f32le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.program.display()))?;
        if !output.status.success() {
            let first = String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            return Err(if first.is_empty() {
                format!("ffmpeg exited with {}", output.status)
            } else {
                first
            });
        }
        let samples = output
            .stdout
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        Ok((channels, samples))
    }

    /// One warm-up decode (kept for the sample comparison), then `runs` timed end-to-end decodes.
    fn time_runs(&self, file: &Path, runs: usize) -> Result<Decode, String> {
        let channels = self.channels(file)?;
        let (_, samples) = self.decode(file, channels)?;
        let frames = samples.len() as u64 / u64::from(channels);
        if frames == 0 {
            return Err("no frames decoded".into());
        }
        let mut ns = Vec::with_capacity(runs);
        for _ in 0..runs {
            let start = Instant::now();
            let decoded = self.decode(file, channels)?;
            black_box(&decoded.1);
            ns.push(start.elapsed().as_nanos() as f64 / frames as f64);
        }
        ns.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs in timings"));
        Ok(Decode {
            frames,
            ns_per_frame: ns[ns.len() / 2],
            samples,
        })
    }
}

/// The `URL SHA-256` pairs of a corpus list; blank lines and `#` comments are skipped.
fn corpus_entries(list: &str) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for (number, line) in list.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [url, digest] = fields[..] else {
            return Err(format!("line {}: expected `URL SHA-256`", number + 1));
        };
        let is_hex =
            digest.len() == 64 && digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !is_hex {
            return Err(format!(
                "line {}: the SHA-256 is not 64 lowercase hex digits",
                number + 1
            ));
        }
        entries.push((url.to_string(), digest.to_string()));
    }
    if entries.is_empty() {
        return Err("the corpus list is empty".into());
    }
    Ok(entries)
}

/// Downloads every file of the corpus `list` into `dir` with curl and verifies its SHA-256.
fn fetch_corpus(list: &Path, dir: &Path) -> Result<Vec<PathBuf>, String> {
    let text =
        fs::read_to_string(list).map_err(|e| format!("cannot read {}: {e}", list.display()))?;
    let mut files = Vec::new();
    for (url, digest) in corpus_entries(&text)? {
        let name = url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("{url}: no file name"))?
            .to_string();
        let path = dir.join(&name);
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
            .arg(&url)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| format!("cannot run curl: {e}"))?;
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

/// `(OS, CPU)`: the Linux distribution and the CPU model where the system tells them.
fn machine() -> (String, String) {
    let read = |path| std::fs::read_to_string(path).unwrap_or_default();
    let (release, cpuinfo) = (read("/etc/os-release"), read("/proc/cpuinfo"));
    let distribution = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .unwrap_or_default();
    let model = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name")?.split(':').nth(1));
    let cpu = (model.map(|m| m.trim().to_string()))
        .or_else(|| first_line(Path::new("sysctl"), &["-n", "machdep.cpu.brand_string"]))
        .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
        .unwrap_or_else(|| "unknown".into());
    let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    (
        // No distribution name outside Linux: no trailing space either.
        format!("{os} {arch} {}", distribution.trim_matches('"'))
            .trim_end()
            .to_string(),
        format!("{cpu}, {threads} logical CPUs"),
    )
}

/// `YYYY-MM-DD` in UTC.
fn utc_today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era_numerator = if z >= 0 { z } else { z - 146_096 };
    let era = era_numerator / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn esc(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// One line of an error, for the rejections sections.
fn trim_err(error: &str) -> String {
    let first = error.lines().next().unwrap_or_default().trim();
    let mut out: String = esc(first);
    if out.chars().count() > 140 {
        out = out.chars().take(140).collect::<String>() + "...";
    }
    out
}

fn fmt_ns(value: f64) -> String {
    match value >= 1000.0 {
        true => format!("{value:.0}"),
        false => format!("{value:.2}"),
    }
}

/// The largest absolute difference of two outputs, or why it is not shown.
fn diff_cell(ak: &Result<Decode, String>, other: &Result<Decode, String>) -> String {
    match (ak, other) {
        (Ok(a), Ok(s)) => {
            if a.frames == s.frames && a.samples.len() == s.samples.len() {
                let d = a
                    .samples
                    .iter()
                    .zip(&s.samples)
                    .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
                if d == 0.0 {
                    "identical".to_string()
                } else {
                    format!("{d:.3e}")
                }
            } else {
                format!("frames differ ({} vs {})", a.frames, s.frames)
            }
        }
        _ => "—".to_string(),
    }
}

fn ns_cell(decode: &Result<Decode, String>) -> Option<f64> {
    decode.as_ref().ok().map(|d| d.ns_per_frame)
}

/// How many of the compared files decoded to bit-identical samples, `identical/total`.
fn bit_identical(
    files: &[FileReport],
    select: impl Fn(&FileReport) -> Option<&Result<Decode, String>>,
) -> String {
    let mut total = 0usize;
    let mut identical = 0usize;
    for f in files {
        let Some(other) = select(f) else { continue };
        match diff_cell(&f.akousma, other).as_str() {
            "identical" => {
                total += 1;
                identical += 1;
            }
            cell if cell != "—" => total += 1,
            _ => {}
        }
    }
    format!("{identical}/{total}")
}

/// The summary of one file row of the capability table.
fn capability_cell(decode: &Result<Decode, String>) -> &'static str {
    match decode {
        Ok(_) => "✔️",
        Err(_) => "❌",
    }
}

/// The row order of the libraries everywhere in the report.
const LIBRARIES: [&str; 4] = ["akousma", "symphonia", "hound", "ffmpeg"];

fn build_report(
    files: &[FileReport],
    runs: usize,
    machine: &(String, String),
    ffmpeg: &Option<Ffmpeg>,
) -> String {
    let n = files.len();
    let (os, cpu) = machine;
    let mut out = String::new();
    out.push_str("# WAVE decoding comparison\n\n");
    out.push_str("Generated by `tools/headtohead` with `--out`; do not edit by hand.\n\n");
    out.push_str(concat!(
        "Results depend on the machine, compiler, library versions and corpus. Compare\n",
        "timings within this report and column. Bold marks the lowest in-process time.\n",
        "The `ffmpeg` column includes process overhead and is not directly comparable.\n",
        "`—` means no result.\n\n",
    ));
    out.push_str(concat!(
        "Corpus files are listed by URL and SHA-256 in `tools/check/corpus.txt` and\n",
        "`tools/headtohead/corpus.txt`. They are downloaded, verified for the run and\n",
        "deleted afterward; they are not committed or distributed. The first list\n",
        "includes files from FFmpeg FATE and Microsoft Multimedia. The second also\n",
        "includes files from `wavefile` (MIT), SDL (zlib) and Microsoft Multimedia. No\n",
        "redistribution licence is known for the Microsoft samples; they are used only\n",
        "for interoperability testing. See `tools/headtohead/PROVENANCE.md` for sources\n",
        "and hashes. `--dir` recursively includes `.wav` and `.w64` files; `--corpus` includes\n",
        "listed files. Inputs with the same basename are deduplicated; avoid collisions.\n\n",
    ));
    out.push_str(concat!(
        "Output differences do not identify which decoder is correct; use the relevant\n",
        "format specification, especially for unusual files. The RIFX samples contain\n",
        "big-endian PCM, while the tested `ffmpeg` CLI reads the PCM payload as\n",
        "little-endian, so those results differ. For IMA ADPCM, akousma uses the\n",
        "shift-add reconstruction from the IMA recommendation; Symphonia and FFmpeg 8.0\n",
        "and earlier use multiplication, while FFmpeg 8.1 and later use shift-add.\n\n",
    ));

    out.push_str("## Environment\n\n");
    out.push_str("| Machine | |\n|---|---|\n");
    out.push_str(&format!("| OS | {} |\n", esc(os)));
    out.push_str(&format!("| CPU | {} |\n", esc(cpu)));
    out.push_str(&format!("| rustc | {} |\n", esc(env!("BENCH_RUSTC"))));
    let libraries = match ffmpeg {
        Some(ffmpeg) => format!("{}, {}", env!("BENCH_LIBRARIES"), esc(&ffmpeg.version)),
        None => format!("{}, ffmpeg not in PATH", env!("BENCH_LIBRARIES")),
    };
    out.push_str(&format!("| Libraries | {libraries} |\n\n"));

    let decoded = |name: &str| -> usize {
        files
            .iter()
            .map(|f| match name {
                "akousma" => f.akousma.is_ok(),
                "symphonia" => f.symphonia.is_ok(),
                "hound" => f.hound.is_ok(),
                _ => f.ffmpeg.as_ref().is_some_and(|r| r.is_ok()),
            })
            .filter(|ok| *ok)
            .count()
    };
    out.push_str("## Coverage\n\n");
    out.push_str("| Library | Decoded |\n|---|---:|\n");
    for name in LIBRARIES {
        match name {
            "ffmpeg" => match ffmpeg {
                Some(_) => out.push_str(&format!("| ffmpeg | {} / {n} |\n", decoded("ffmpeg"))),
                None => out.push_str("| ffmpeg | not in PATH |\n"),
            },
            _ => out.push_str(&format!("| {name} | {} / {n} |\n", decoded(name))),
        }
    }
    out.push('\n');

    out.push_str("## Capability by file\n\n");
    out.push_str("✔️ decoded · ❌ rejected.\n\n");
    out.push_str("| File | akousma | symphonia | hound | ffmpeg |\n|---|:-:|:-:|:-:|:-:|\n");
    for f in files {
        let ffmpeg_cell = match &f.ffmpeg {
            Some(result) => capability_cell(result),
            None => "—",
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            esc(&f.name),
            capability_cell(&f.akousma),
            capability_cell(&f.symphonia),
            capability_cell(&f.hound),
            ffmpeg_cell,
        ));
    }
    out.push('\n');

    out.push_str("## Decoding speed\n\n");
    out.push_str(
        "nanoseconds per frame, lower is better. The `ffmpeg` column is end to end (process start-up, reading a temporary file, decoding and the pipe) and is not comparable with the in-process rows. `—` means the library did not decode the file.\n\n",
    );
    out.push_str("| File | akousma | symphonia | hound | ffmpeg |\n|---|---:|---:|---:|---:|\n");
    for f in files {
        let cells = [ns_cell(&f.akousma), ns_cell(&f.symphonia), ns_cell(&f.hound)];
        let best = cells.iter().flatten().copied().reduce(f64::min);
        let speed_cell = |value: Option<f64>| match value {
            Some(v) => match best == Some(v) {
                true => format!("**{}**", fmt_ns(v)),
                false => fmt_ns(v),
            },
            None => "—".to_string(),
        };
        let ffmpeg_cell = match &f.ffmpeg {
            Some(result) => match ns_cell(result) {
                Some(v) => fmt_ns(v),
                None => "—".to_string(),
            },
            None => "—".to_string(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            esc(&f.name),
            speed_cell(cells[0]),
            speed_cell(cells[1]),
            speed_cell(cells[2]),
            ffmpeg_cell,
        ));
    }
    out.push('\n');
    let libraries: [(&str, fn(&FileReport) -> &Result<Decode, String>); 2] = [
        ("symphonia", |f| &f.symphonia),
        ("hound", |f| &f.hound),
    ];
    for (name, select) in libraries {
        let mut ratios: Vec<f64> = files
            .iter()
            .filter_map(|f| {
                let ak = f.akousma.as_ref().ok()?;
                let other = select(f).as_ref().ok()?;
                Some(other.ns_per_frame / ak.ns_per_frame)
            })
            .collect();
        if ratios.is_empty() {
            out.push_str(&format!("- `{name}`: no file both decoded.\n"));
        } else {
            ratios.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs in ratios"));
            let median = ratios[ratios.len() / 2];
            out.push_str(&format!(
                "- `{name}`: median `{name} / akousma` ratio (ns per frame) {median:.2}, on {} file(s) both decoded.\n",
                ratios.len()
            ));
        }
    }
    out.push('\n');
    out.push_str("## Sample difference\n\n");
    out.push_str(
        "The largest absolute difference of the interleaved `f32` outputs against akousma. `identical` means bit-equal samples; `frames differ (a vs b)` means the libraries disagreed on how many frames the file holds (akousma trims the ADPCM padding to the `fact` count); `—` means akousma did not decode the file.\n\n",
    );
    out.push_str("| File | symphonia | hound | ffmpeg |\n|---|---|---|---|\n");
    for f in files {
        let ffmpeg_cell = match &f.ffmpeg {
            Some(result) => diff_cell(&f.akousma, result),
            None => "—".to_string(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            esc(&f.name),
            diff_cell(&f.akousma, &f.symphonia),
            diff_cell(&f.akousma, &f.hound),
            ffmpeg_cell,
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "Bit-identical outputs: `symphonia` {}, `hound` {}{}.\n\n",
        bit_identical(files, |f: &FileReport| Some(&f.symphonia)),
        bit_identical(files, |f: &FileReport| Some(&f.hound)),
        match ffmpeg {
            Some(_) => format!(", `ffmpeg` {}", bit_identical(files, |f: &FileReport| f.ffmpeg.as_ref())),
            None => String::new(),
        }
    ));

    let mut rejections: Vec<(&str, Vec<(String, String)>)> = Vec::new();
    for name in LIBRARIES {
        let mut rows = Vec::new();
        for f in files {
            let error = match name {
                "akousma" => f.akousma.as_ref().err(),
                "symphonia" => f.symphonia.as_ref().err(),
                "hound" => f.hound.as_ref().err(),
                _ => match &f.ffmpeg {
                    Some(result) => result.as_ref().err(),
                    None => None,
                },
            };
            if let Some(error) = error {
                rows.push((f.name.clone(), error.clone()));
            }
        }
        if !rows.is_empty() {
            rejections.push((name, rows));
        }
    }
    if !rejections.is_empty() {
        out.push_str("## Rejections\n\n");
        for (name, rows) in &rejections {
            out.push_str(&format!("### `{name}`\n\n"));
            for (name, error) in rows {
                out.push_str(&format!("- `{}`: {}\n", esc(name), trim_err(error)));
            }
            out.push('\n');
        }
    }

    out.push_str("## How to read\n\n");
    out.push_str(&format!(
        "- nanoseconds per frame is the time of a whole file divided by the number of frames the decoder produced; every value is the median of {runs} timed decodes after one warm-up decode; the in-process libraries are timed one after another within each iteration and the starting library rotates every iteration, except the `ffmpeg` column, whose `--runs` are end to end. A difference of a tenth or less on a single row is within machine noise and should be read as a tie.\n"
    ));
    out.push_str("- Ratios are other / akousma nanoseconds per frame, computed only over files both libraries decoded; a ratio below 1 means the other library took less time. Bold compares the in-process values within each column.\n");
    out.push_str("- The `ffmpeg` numbers include the process start-up, the reading of a temporary file, the decoding, the conversion and the pipe, so they are not comparable with the in-process rows and are excluded from the ratios.\n");
    out.push_str("- `frames differ` is not an error: one library trimmed the ADPCM block padding to the `fact` count and the other did not.\n");
    out.push_str(&format!(
        "- Generated on {} from {n} file(s); pass the same `--dir`, `--runs` and `--out` to reproduce.\n",
        utc_today()
    ));
    out.push_str("\n*Machine-specific snapshot. Do not edit.*\n");
    out
}

struct Args {
    dirs: Vec<PathBuf>,
    corpus: Vec<PathBuf>,
    ffmpeg: Option<PathBuf>,
    runs: usize,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut dirs = Vec::new();
    let mut corpus = Vec::new();
    let mut ffmpeg = None;
    let mut runs = 100usize;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dirs.push(PathBuf::from(
                args.next().ok_or("--dir needs a value")?,
            )),
            "--corpus" => corpus.push(PathBuf::from(
                args.next().ok_or("--corpus needs a value")?,
            )),
            "--ffmpeg" => {
                if ffmpeg.is_some() {
                    return Err("--ffmpeg given twice".into());
                }
                ffmpeg = Some(PathBuf::from(
                    args.next().ok_or("--ffmpeg needs a value")?,
                ));
            }
            "--runs" => {
                let value = args.next().ok_or("--runs needs a value")?;
                runs = value
                    .parse::<usize>()
                    .ok()
                    .filter(|runs| *runs > 0)
                    .ok_or_else(|| format!("--runs: not a positive integer: {value}"))?;
            }
            "--out" => {
                if out.is_some() {
                    return Err("--out given twice".into());
                }
                out = Some(PathBuf::from(args.next().ok_or("--out needs a value")?));
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if dirs.is_empty() && corpus.is_empty() {
        return Err(format!("no --dir or --corpus given\n{USAGE}"));
    }
    Ok(Args {
        dirs,
        corpus,
        ffmpeg,
        runs,
        out,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let temp_root =
        std::env::temp_dir().join(format!("akousma-headtohead-{}", std::process::id()));
    let outcome = work(&args, &temp_root);
    if temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }
    outcome
}

fn work(args: &Args, temp_root: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    for dir in &args.dirs {
        collect(dir, &mut files);
    }
    if !args.corpus.is_empty() {
        let corpus_dir = temp_root.join("corpus");
        fs::create_dir_all(&corpus_dir)
            .map_err(|e| format!("cannot create {}: {e}", corpus_dir.display()))?;
        for list in &args.corpus {
            files.extend(fetch_corpus(list, &corpus_dir)?);
        }
    }
    // Inputs with the same basename are deduplicated, including across directories and manifests.
    let mut seen = std::collections::HashSet::new();
    files.retain(|path| {
        let key = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        seen.insert(key)
    });
    files.sort();

    let ffmpeg = Ffmpeg::find(args.ffmpeg.clone());
    if ffmpeg.is_none() {
        eprintln!("akousma-headtohead: ffmpeg not found; the ffmpeg rows are `—`");
    }

    let mut temp_made = temp_root.exists();
    let mut reports = Vec::with_capacity(files.len());
    for (index, path) in files.iter().enumerate() {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("akousma-headtohead: cannot read {}: {e}", path.display());
                continue;
            }
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let [akousma, symphonia, hound] =
            time_runs_interleaved([akousma_once, symphonia_once, hound_once], &bytes, args.runs);
        let ffmpeg_result = ffmpeg.as_ref().map(|ff| {
            if !temp_made {
                if let Err(e) = fs::create_dir_all(temp_root) {
                    return Err(format!("cannot create {}: {e}", temp_root.display()));
                }
                temp_made = true;
            }
            let tmp = temp_root.join(format!("input-{index}.wav"));
            match fs::write(&tmp, &bytes) {
                Ok(()) => ff.time_runs(&tmp, args.runs),
                Err(e) => Err(format!("cannot write {}: {e}", tmp.display())),
            }
        });
        reports.push(FileReport {
            name,
            akousma,
            symphonia,
            hound,
            ffmpeg: ffmpeg_result,
        });
    }
    let machine = machine();
    let report = build_report(&reports, args.runs, &machine, &ffmpeg);
    match &args.out {
        Some(path) => {
            fs::write(path, report).map_err(|e| format!("cannot write {}: {e}", path.display()))?
        }
        None => print!("{report}"),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("akousma-headtohead: {error}");
        std::process::exit(1);
    }
}
