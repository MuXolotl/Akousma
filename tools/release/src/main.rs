//! Version bump for a release of the `akousma` workspace.
//!
//! `--version X` is the only code path that writes the version into the
//! manifests and the changelog; a release job never edits a manifest with
//! `sed`. The exact rewrites and the refused inputs are in `README.md`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PACKAGE_DIR: &str = env!("CARGO_MANIFEST_DIR");
const CHANGELOG: &str = "CHANGELOG.md";
const ROOT_MANIFEST: &str = "Cargo.toml";
const CRATES_DIR: &str = "crates";
const UNRELEASED: &str = "[Unreleased]";
// Each of these packages keeps its own committed lock file, built with
// `--locked` in CI: it pins the workspace crates by version, so a bump that
// leaves them behind fails the next check with "lock file needs to be
// updated". They are patched here, where both versions are known.
const TOOL_PACKAGES: [&str; 2] = ["tools/benchmarks", "tools/headtohead"];
/// The repository every changelog link points at.
const REPOSITORY: &str = "https://github.com/MuXolotl/Akousma";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            err.report();
            ExitCode::from(err.code)
        }
    }
}

#[derive(Debug)]
struct Failure {
    message: String,
    code: u8,
}

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 2,
        }
    }
    fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 1,
        }
    }
    fn report(&self) {
        eprintln!("akousma-release: {}", self.message);
    }
}

type Result<T> = std::result::Result<T, Failure>;

fn run(args: &[String]) -> Result<()> {
    let mut version = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| Failure::usage(format!("`{flag}` needs a value")))?;
        match flag {
            "--version" => version = Some(Version::parse(value)?),
            other => return Err(Failure::usage(format!("unknown option `{other}`"))),
        }
        index += 2;
    }
    let version = version.ok_or_else(|| Failure::usage("`--version` is required"))?;
    prepare(&repository_root()?, &version)
}

/// `MAJOR.MINOR.PATCH`, optionally followed by `-PRERELEASE`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version(String);

impl Version {
    fn parse(text: &str) -> Result<Self> {
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        let numeric = parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.chars().all(|c| c.is_ascii_digit())
                    && (part == &"0" || !part.starts_with('0'))
            });
        if !numeric {
            return Err(Failure::usage(format!(
                "`{text}` is not a version of the form MAJOR.MINOR.PATCH[-PRERELEASE]"
            )));
        }
        if let Some(pre) = pre {
            let ok = !pre.is_empty()
                && pre
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if !ok {
                return Err(Failure::usage(format!(
                    "`{text}` has an invalid pre-release part"
                )));
            }
        }
        Ok(Self(text.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The current directory when it is the root of this workspace, else two
/// levels above the package: a copied binary runs against a checkout.
fn repository_root() -> Result<PathBuf> {
    if let Ok(dir) = std::env::current_dir()
        && looks_like_root(&dir)
    {
        return Ok(dir);
    }
    let root = Path::new(PACKAGE_DIR)
        .ancestors()
        .nth(2)
        .ok_or_else(|| Failure::usage("cannot find the repository root from CARGO_MANIFEST_DIR"))?;
    if !looks_like_root(root) {
        return Err(Failure::usage(format!(
            "`{}` does not look like the repository root; run me from it",
            root.display()
        )));
    }
    Ok(root.to_path_buf())
}

fn looks_like_root(dir: &Path) -> bool {
    dir.join(ROOT_MANIFEST).is_file()
        && dir.join(CHANGELOG).is_file()
        && dir.join(CRATES_DIR).is_dir()
}

struct Member {
    name: String,
    path: PathBuf,
    text: String,
}

fn members(root: &Path) -> Result<Vec<Member>> {
    let mut found = Vec::new();
    let dir = root.join(CRATES_DIR);
    let entries = fs::read_dir(&dir)
        .map_err(|err| Failure::failed(format!("cannot read `{}`: {err}", dir.display())))?;
    for entry in entries {
        let path = entry
            .map_err(|err| Failure::failed(format!("cannot read `{}`: {err}", dir.display())))?
            .path()
            .join(ROOT_MANIFEST);
        if !path.is_file() {
            continue;
        }
        let text = fs::read_to_string(&path)
            .map_err(|err| Failure::failed(format!("cannot read `{}`: {err}", path.display())))?;
        let name = package_name(&text).ok_or_else(|| {
            Failure::failed(format!("`{}` has no `[package] name`", path.display()))
        })?;
        found.push(Member { name, path, text });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    if found.is_empty() {
        return Err(Failure::failed(format!(
            "no member manifests under `{}`",
            dir.display()
        )));
    }
    Ok(found)
}

fn package_name(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some(value) = line.strip_prefix("name")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn workspace_version(text: &str) -> Result<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[workspace.package]";
            continue;
        }
        if in_section
            && let Some(value) = line.strip_prefix("version")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            let value = value.trim().trim_matches('"');
            Version::parse(value).map_err(|_| {
                Failure::failed(format!(
                    "`[workspace.package] version = \"{value}\"` is invalid"
                ))
            })?;
            return Ok(value.to_owned());
        }
    }
    Err(Failure::failed(
        "the root Cargo.toml has no `[workspace.package] version`",
    ))
}

fn set_workspace_version(text: &str, old: &str, new: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut in_section = false;
    let mut done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
        } else if in_section
            && !done
            && let Some(rest) = trimmed.strip_prefix("version")
            && let Some(rest) = rest.trim_start().strip_prefix('=')
            && rest.trim().trim_matches('"') == old
        {
            let _ = writeln!(out, "version = \"{new}\"");
            done = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !done {
        return Err(Failure::failed(format!(
            "cannot find `version = \"{old}\"` in `[workspace.package]`"
        )));
    }
    Ok(out)
}

struct Requirement {
    name: String,
    start: usize,
    end: usize,
    old: String,
}

/// The package a `[dependencies]` line names. The key alone is not enough:
/// `akousma` is a prefix of `akousma-core`, and a renamed dependency names
/// its package in `package = "..."` or in the last component of `path`.
fn dependency_name(key: &str, value: &str) -> Option<String> {
    for field in ["package", "path"] {
        let needle = format!("{field} = ");
        if let Some(at) = value.find(&needle) {
            let rest = value[at + needle.len()..].trim_start_matches('"');
            let end = rest.find('"').unwrap_or(rest.len());
            let raw = &rest[..end];
            let name = if field == "package" {
                raw
            } else {
                raw.rsplit('/').next().unwrap_or(raw)
            };
            return Some(name.to_owned());
        }
    }
    Some(key.to_owned())
}

/// Every `version = "<old>"` of a dependency on a workspace member, with the
/// byte range of the version string inside `text`.
fn requirements(text: &str, member_names: &[String]) -> Vec<Requirement> {
    let mut found = Vec::new();
    let mut section = String::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.trim_matches(|c| c == '[' || c == ']').to_owned();
        } else if (section == "dependencies" || section.ends_with(".dependencies"))
            && let Some((key, value)) = trimmed.split_once('=')
            && let Some(dependency) = dependency_name(key.trim(), value)
            && member_names.contains(&dependency)
            && let Some(at) = line.find("version")
        {
            let after = &line[at + "version".len()..];
            let eq = after.find('=').map(|index| at + "version".len() + index);
            if let Some(eq) = eq
                && let Some(open) = line[eq..].find('"').map(|index| eq + index)
                && let Some(close) = line[open + 1..].find('"').map(|index| open + 1 + index)
            {
                found.push(Requirement {
                    name: dependency,
                    start: offset + open + 1,
                    end: offset + close,
                    old: line[open + 1..close].to_owned(),
                });
            }
        }
        offset += line.len();
    }
    found
}

/// Byte offsets are computed on the LF text that is written back, so a CRLF
/// checkout is normalised first.
/// Rewrites the `version` of every workspace crate inside a committed lock
/// file.
///
/// A lock file names a path package by name and version only, with no source
/// and no checksum, so the line after the name is the whole record. Only those
/// records change: a third-party package that happens to share a name keeps
/// its version.
fn set_lock_versions(text: &str, names: &[String], version: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_name: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(name) = trimmed
            .strip_prefix("name = \"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            previous_name = Some(name);
            out.push_str(line);
            continue;
        }
        let is_workspace_crate = previous_name.is_some_and(|name| names.iter().any(|n| n == name));
        if is_workspace_crate && trimmed.starts_with("version = \"") {
            out.push_str(&format!("version = \"{version}\"\n"));
        } else {
            out.push_str(line);
        }
        previous_name = None;
    }
    out
}

fn lf_only(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn prepare(root: &Path, version: &Version) -> Result<()> {
    if !clean(root)? {
        return Err(Failure::failed(
            "the working tree is not clean; commit or stash first",
        ));
    }
    let version = version.as_str();
    let root_path = root.join(ROOT_MANIFEST);
    let root_text = read(&root_path)?;
    let old = workspace_version(&root_text)?;

    let crates = members(root)?;
    let names: Vec<String> = crates.iter().map(|m| m.name.clone()).collect();

    let mut edits: Vec<(PathBuf, String)> = Vec::new();
    if old != version {
        edits.push((
            root_path.clone(),
            set_workspace_version(&root_text, &old, version)?,
        ));
    }
    for member in &crates {
        let text = lf_only(&member.text);
        let mut updated = text.clone();
        for requirement in requirements(&text, &names).iter().rev() {
            if requirement.old == old || requirement.old.trim() == "0" {
                updated.replace_range(requirement.start..requirement.end, version);
            } else {
                return Err(Failure::failed(format!(
                    "`{}` requires `{}` as `{}`, but the workspace version is `{old}`",
                    relative(root, &member.path),
                    requirement.name,
                    requirement.old
                )));
            }
        }
        if updated != text {
            edits.push((member.path.clone(), updated));
        }
    }

    if old != version {
        for package in TOOL_PACKAGES {
            let path = root.join(package).join("Cargo.lock");
            if !path.exists() {
                continue;
            }
            let text = read(&path)?;
            let updated = set_lock_versions(&text, &names, version);
            if updated != text {
                edits.push((path, updated));
            }
        }
    }

    let changelog_path = root.join(CHANGELOG);
    let changelog = read(&changelog_path)?;
    edits.push((changelog_path, date_changelog(&changelog, version)?));

    for (path, text) in &edits {
        fs::write(path, text)
            .map_err(|err| Failure::failed(format!("cannot write `{}`: {err}", path.display())))?;
        println!("wrote {}", relative(root, path));
    }
    println!(
        "version {version}{}",
        if version.contains('-') {
            " (pre-release)"
        } else {
            ""
        }
    );
    Ok(())
}

fn line_offset_where(text: &str, matches: impl Fn(&str) -> bool) -> Option<usize> {
    text.lines()
        .scan(0, |offset, line| {
            let at = *offset;
            *offset += line.len() + 1;
            Some((at, line))
        })
        .find(|(_, line)| matches(line))
        .map(|(at, _)| at)
}

/// `[Unreleased]` becomes a dated section under a fresh empty `[Unreleased]`,
/// the two are separated by `---`, and the links footer at the end of the
/// file compares from the new tag.
fn date_changelog(changelog: &str, version: &str) -> Result<String> {
    let heading = format!("## {UNRELEASED}");
    let start =
        line_offset_where(changelog, |line| line.trim_end() == heading).ok_or_else(|| {
            Failure::failed(format!("`{CHANGELOG}` has no `## {UNRELEASED}` heading"))
        })?;

    let after_heading = start + heading.len();
    let tail = &changelog[after_heading..];
    let body_start = tail.find('\n').map_or(tail.len(), |index| index + 1);
    let body_and_after = &tail[body_start..];
    let end = body_and_after
        .lines()
        .scan(0, |offset, line| {
            let at = *offset;
            *offset += line.len() + 1;
            Some((at, line))
        })
        .find(|(_, line)| line.starts_with("## ") || *line == "---")
        .map_or(body_and_after.len(), |(at, _)| at);
    let body = &body_and_after[..end];
    if body.trim().is_empty() {
        return Err(Failure::failed(format!(
            "`{CHANGELOG}`: `{UNRELEASED}` is empty; nothing to release"
        )));
    }
    let released = format!("## [{version}]");
    if line_offset_where(changelog, |line| line.starts_with(&released)).is_some() {
        return Err(Failure::failed(format!(
            "`{CHANGELOG}` already has a `[{version}]` section"
        )));
    }

    // The section rule under `[Unreleased]` is not part of the sections below
    // it: it comes back in the output, written between the two sections.
    let mut after_body = &body_and_after[end..];
    match after_body.find('\n') {
        Some(line_end) if after_body[..line_end].trim_end() == "---" => {
            after_body = &after_body[line_end + 1..];
        }
        None if after_body.trim_end() == "---" => after_body = "",
        _ => {}
    }
    let rest = after_body.trim();

    let mut out = String::with_capacity(changelog.len() + body.len());
    out.push_str(&changelog[..start]);
    let _ = write!(
        out,
        "## {UNRELEASED}\n\n---\n\n## [{version}] — {}\n\n{}",
        today()?,
        body.trim()
    );
    if !rest.is_empty() {
        let _ = write!(out, "\n\n---\n\n{rest}");
    }
    out.push('\n');
    Ok(links_footer(&out, version))
}

/// `[Unreleased]` compares from the new tag; the new version gets its first
/// link right under it, above the links of older releases.
fn links_footer(text: &str, version: &str) -> String {
    let unreleased = format!("[Unreleased]: {REPOSITORY}/compare/v{version}...HEAD");
    let released = format!("[{version}]: {REPOSITORY}/releases/tag/v{version}");
    let known = format!("[{version}]: ");
    if let Some(at) = line_offset_where(text, |line| line.starts_with("[Unreleased]: ")) {
        let line_end = text[at..]
            .find('\n')
            .map_or(text.len(), |index| at + index + 1);
        let mut out = String::with_capacity(text.len() + released.len());
        out.push_str(&text[..at]);
        out.push_str(&unreleased);
        out.push('\n');
        if line_offset_where(text, |line| line.starts_with(&known)).is_none() {
            out.push_str(&released);
            out.push('\n');
        }
        out.push_str(&text[line_end..]);
        out
    } else {
        let mut out = text.trim_end().to_string();
        if out.rsplit('\n').next() == Some("---") {
            out.push_str("\n\n");
        } else {
            out.push_str("\n\n---\n\n");
        }
        let _ = writeln!(out, "{unreleased}");
        let _ = writeln!(out, "{released}");
        out
    }
}

/// Today in UTC, as `YYYY-MM-DD`.
fn today() -> Result<String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| Failure::failed(format!("the system clock is before 1970: {err}")))?
        .as_secs();
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Days since 1970-01-01 to a Gregorian date (Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn clean(root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|err| Failure::failed(format!("cannot run git: {err}")))?;
    if !output.status.success() {
        return Err(Failure::failed("`git status` failed"));
    }
    Ok(output.stdout.iter().all(u8::is_ascii_whitespace))
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map_err(|err| Failure::failed(format!("cannot read `{}`: {err}", path.display())))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        ["akousma", "akousma-core", "akousma-meta", "akousma-wav"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    #[test]
    fn versions() {
        assert!(Version::parse("0.1.0").is_ok());
        assert!(Version::parse("1.2.3-beta.1").is_ok());
        assert!(Version::parse("0.1").is_err());
        assert!(Version::parse("01.1.0").is_err());
        assert!(Version::parse("1.2.3-").is_err());
        assert!(Version::parse("v1.2.3").is_err());
        assert!(
            Version::parse("1.2.3-beta.1")
                .unwrap()
                .as_str()
                .contains('-')
        );
    }

    #[test]
    fn workspace_version_is_read_and_rewritten() {
        let text =
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.1.0\"\n";
        assert_eq!(workspace_version(text).unwrap(), "0.1.0");
        let updated = set_workspace_version(text, "0.1.0", "0.2.0").unwrap();
        assert!(updated.contains("version = \"0.2.0\""));
        assert!(updated.contains("members"));
        assert!(set_workspace_version(text, "9.9.9", "1.0.0").is_err());
    }

    #[test]
    fn requirements_are_found_with_exact_offsets() {
        let text = "[package]\nname = \"akousma-wav\"\n\n[dependencies]\nakousma-core = { version = \"0.1.0\", path = \"../akousma-core\" }\nakousma-meta = { version = \"0.1.0\", path = \"../akousma-meta\" }\nsymphonia = { version = \"0.6.1\" }\n";
        let found = requirements(text, &names());
        assert_eq!(found.len(), 2);
        for requirement in &found {
            assert_eq!(requirement.old, "0.1.0");
            assert_eq!(&text[requirement.start..requirement.end], "0.1.0");
        }
    }

    #[test]
    fn a_foreign_requirement_is_not_touched() {
        let text = "[dependencies]\nsymphonia = { version = \"0.6.1\" }\n";
        assert!(requirements(text, &names()).is_empty());
    }

    #[test]
    fn a_requirement_is_named_by_its_package_or_path() {
        assert_eq!(
            dependency_name(
                "akousma_core",
                "{ version = \"0.1.0\", package = \"akousma-core\" }"
            ),
            Some("akousma-core".to_owned())
        );
        assert_eq!(
            dependency_name("wav", "{ version = \"0.1.0\", path = \"../akousma-wav\" }"),
            Some("akousma-wav".to_owned())
        );
        let text = "[dependencies]\nakousma = { version = \"0.1.0\", path = \"../akousma\" }\nakousma-core = { version = \"0.1.0\", path = \"../akousma-core\" }\n";
        assert_eq!(requirements(text, &names()).len(), 2);
    }

    #[test]
    fn changelog_gets_a_dated_section_and_a_fresh_unreleased() {
        let changelog = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- One thing.\n\n## [0.0.9] — 2026-01-01\n\n- Old.\n";
        let out = date_changelog(changelog, "0.1.0").unwrap();
        assert!(out.contains("## [Unreleased]\n\n---\n\n## [0.1.0] — "));
        assert!(out.contains("### Added\n\n- One thing."));
        assert!(out.contains("- One thing.\n\n---\n\n## [0.0.9] — 2026-01-01"));
        assert!(!out.contains("One thing.## "));
        assert!(
            out.contains("[Unreleased]: https://github.com/MuXolotl/Akousma/compare/v0.1.0...HEAD")
        );
        assert!(out.contains("[0.1.0]: https://github.com/MuXolotl/Akousma/releases/tag/v0.1.0"));
    }

    #[test]
    fn a_dated_section_behind_the_rule_rewrites_the_links_and_keeps_the_old_one() {
        let changelog = "# Changelog\n\n---\n\n## [Unreleased]\n\n- New thing.\n\n---\n\n## [0.1.0] — 2026-11-23\n\n- Old thing.\n\n---\n\n[Unreleased]: https://github.com/MuXolotl/Akousma/compare/v0.1.0...HEAD\n[0.1.0]: https://github.com/MuXolotl/Akousma/releases/tag/v0.1.0\n";
        let out = date_changelog(changelog, "0.2.0").unwrap();
        assert!(out.contains("## [Unreleased]\n\n---\n\n## [0.2.0] — "));
        assert!(out.contains("- New thing.\n\n---\n\n## [0.1.0] — 2026-11-23"));
        assert!(
            out.contains("[Unreleased]: https://github.com/MuXolotl/Akousma/compare/v0.2.0...HEAD")
        );
        assert!(out.contains("[0.2.0]: https://github.com/MuXolotl/Akousma/releases/tag/v0.2.0"));
        assert!(out.contains("[0.1.0]: https://github.com/MuXolotl/Akousma/releases/tag/v0.1.0"));
        assert!(!out.contains("compare/v0.1.0...HEAD"));
    }

    #[test]
    fn an_empty_unreleased_is_refused_and_an_existing_release_too() {
        let empty = "## [Unreleased]\n\n";
        assert!(date_changelog(empty, "0.1.0").is_err());
        let done = "## [Unreleased]\n\n- x\n\n## [0.1.0] — 2026-11-21\n\n- y\n";
        assert!(date_changelog(done, "0.1.0").is_err());
    }

    #[test]
    fn lock_versions_of_workspace_crates_only() {
        let lock = "version = 4\n\
\n\
[[package]]\n\
name = \"akousma-wav\"\n\
version = \"0.1.0\"\n\
dependencies = [\n\
 \"akousma-core\",\n\
]\n\
\n\
[[package]]\n\
name = \"extended\"\n\
version = \"0.1.0\"\n\
source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        let out = set_lock_versions(lock, &names(), "0.1.1");
        assert!(out.contains("name = \"akousma-wav\"\nversion = \"0.1.1\""));
        // A third-party crate that happens to share a version is not ours.
        assert!(out.contains("name = \"extended\"\nversion = \"0.1.0\""));
        assert_eq!(out.matches("version = \"0.1.1\"").count(), 1);
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_654), (2026, 7, 20));
        assert!(today().unwrap().len() == 10);
    }
}
