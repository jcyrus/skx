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
