# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Agent Skills spec compliance.** `license`, `compatibility`,
  `allowed-tools` and `metadata` are now parsed, and any frontmatter key skx
  doesn't model is preserved verbatim through a round-trip.
- **Skill table** with author, version and approximate context cost, plus a
  description column and a per-target status column.
- **Markdown-rendered preview**, with headings, lists, emphasis and
  syntax-highlighted code blocks.
- **Filtering** with `/` — fuzzy on names, literal substring on descriptions
  and authors.
- **Light and dark themes**, selectable with `--theme` or `SKX_THEME`, plus
  `NO_COLOR` support and 256/16-colour fallbacks.
- **Mouse support** — click to focus and select, wheel to scroll.
- **Background sync** on a worker thread with a determinate progress bar.
- **Quit confirmation**, always shown when work is unsynced or a sync is
  running, and configurable for the quiet case.
- **Config file** at `~/.config/skx/config.toml`.
- **Artifacts pane** showing where the selected skill was actually written.
- `Ctrl-Z` suspends and resumes cleanly instead of leaving the terminal in
  raw mode.

### Changed

- **A skill is now a directory, not a file.** Symlinking targets link the
  whole skill directory rather than just `SKILL.md`, so `scripts/`,
  `references/` and `assets/` travel with it. `skx add` copies those spec
  directories into the cache; `skx export` mirrors them as real files.

  Workspaces synced by an earlier version are migrated automatically on the
  next `skx sync`. Any file sitting in the destination directory that skx
  didn't write — a hand-edited `references/`, an agent's own state file — is
  copied into the cache before the directory is replaced, and each rescued
  file is reported. Nothing is deleted silently.

### Fixed

- Installing a spec-compliant skill silently destroyed its `license`,
  `compatibility`, `allowed-tools` and `metadata` — including the author —
  and reset an explicit version to the `0.1.0` default. Because skx symlinks
  its rendered copy over the original, that loss was unrecoverable.
- The palette was authored for a dark terminal and no background was ever
  painted, so on a light profile eight of ten colour tokens fell below WCAG
  minimums, with body text at 1.55:1.
- The table header and the selected row shared a background colour and were
  indistinguishable.
- Syncing blocked the render loop, freezing the interface for the duration
  with no progress indication.
- `Esc` quit the application when no filter was set.
- `Space` refused to toggle a target unless the Agent Matrix already held
  focus, despite the footer advertising it unconditionally.
- Skill names were validated against skx's own rules rather than the spec's:
  up to 128 characters instead of 64, and consecutive hyphens were accepted.
  Both would install a skill the downstream agents then reject.
- Bundled `scripts/`, `references/` and `assets/` were left behind entirely:
  only `SKILL.md` was cached and linked, so every relative file reference in
  a skill body dangled the moment it was exported or synced anywhere other
  than its original directory.
- `skx export` resolved a skill's cache location from the output scope it
  had just overridden, so exporting a globally-installed skill looked for it
  in a local cache that had never existed.
- Syncing on Windows failed outright for accounts without Developer Mode or
  an elevated prompt, because every symlink creation was rejected. skx now
  falls back to a copy there and records it as one, so drift detection stays
  accurate.
