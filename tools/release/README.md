# akousma-release

Sets one version across the workspace and dates the changelog. Part of
`tools/`, not published.

```sh
cargo run --release --manifest-path tools/release/Cargo.toml -- --version 0.1.0
```

Run it from the repository root on a clean tree. It rewrites
`[workspace.package] version`, the `version` of every inter-crate dependency,
turns `## [Unreleased]` into `## [0.1.0] — <today>` separated from a fresh
empty `## [Unreleased]` above it by `---`, and rewrites the links footer so
`[Unreleased]` compares from `v0.1.0` and the new version links at its tag.

It refuses a dirty tree, an empty `[Unreleased]`, a version that already has a
section, and a version that is not `MAJOR.MINOR.PATCH[-PRERELEASE]`. A
requirement that does not match the current workspace version stops the run
instead of being overwritten.
