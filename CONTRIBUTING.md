# Contributing to skx

Thanks for taking a look. This is a small project with a small surface, so
the bar is mostly "does it hold up in review" rather than a heavy process.

## Getting set up

```bash
git clone https://github.com/jcyrus/skx
cd skx
cargo test
cargo run -- --help
```

Minimum supported Rust is **1.88** — edition 2024 needs 1.85, and the
let-chains used throughout need 1.88. CI checks this separately from the
stable build, so bumping it is a deliberate change, not an accident.

`skx.toml` and `.skx/` are gitignored on purpose. This repository is itself
a skx workspace because we dogfood the tool, and those files record which
skills are installed *on your machine*, with absolute paths into your home
directory. They are not project files.

## Before opening a pull request

```bash
cargo fmt --all
cargo clippy --all-targets --all-features   # must be warning-free
cargo test
```

CI runs all three on Linux, macOS and Windows. The matrix is about skx's own
platform-specific code — cache path resolution and symlink creation — rather
than about Rust portability in general, so a change touching either deserves
a look at all three.

## What review looks for

**Tests that describe the bug, not the code.** A test name should say what
would break. `slack_width_is_spent_on_the_description_not_left_empty` earns
its length; `test_layout` does not.

**Comments that explain why.** The code already says what it does. Comments
are for the reasoning that isn't recoverable from reading it — a constraint,
a rejected alternative, a bug that motivated an odd-looking line.

**Contrast is enforced, not eyeballed.** `crates/skx_tui/src/theme.rs` has
tests that recompute every WCAG ratio at build time. If you add a colour
token, it has to pass them. This exists because the palette was once
authored against a dark terminal and was unreadable on a light one, with
nothing to catch it.

**Rendering changes need a render test.** `crates/skx_tui/src/ui.rs` uses
ratatui's `TestBackend` to assert against a real frame buffer. Layout
regressions are easy to introduce and invisible in a diff.

There is also a headless screenshot helper for eyeballing layout without
launching a terminal session:

```bash
cargo run -p skx_tui --example screenshot -- 120 34
cargo run -p skx_tui --example screenshot -- 120 34 "?"   # with the help overlay
```

## Architecture in one paragraph

`skx_core` owns the canonical `SKILL.md` schema, parsing, drift detection
and discovery, and knows nothing about any particular agent. `skx_adapters`
holds one compilation target per agent and is where per-target config in
`frontmatter.targets` is finally interpreted — core keeps that map
deliberately opaque so adding an agent never touches core. `skx_tui` is the
cockpit: a testable `App` data model with a separate pure rendering layer.
`skx_cli` is thin orchestration over the other three.

## Adding an agent target

Implement `SkillAdapter` in `skx_adapters`, register it in
`default_adapters()`, and add it to `MATRIX_COLUMNS` in
`crates/skx_tui/src/app.rs` with a two-letter code in `matrix_code()`.
Single letters won't do — claude, cursor and copilot all start with `C`.

## Reporting bugs

Include your terminal and its `TERM`/`COLORTERM` values for anything
visual, and the output of `skx audit` for anything about sync or drift.
