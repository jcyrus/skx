//! Generic write primitives shared by every `skx_adapters::SkillAdapter`.
//!
//! These types deliberately carry no agent-specific knowledge — they only
//! describe *how much of a destination file* an adapter owns. That's what
//! lets [`crate::sync`] host a single write engine that materializes any
//! adapter's output, and what lets [`crate::state`] fingerprint any
//! artifact the same way regardless of which target produced it.

use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

/// Whether an adapter's compiled output can be a zero-copy symlink back to
/// the canonical skill, or must always be written out.
///
/// Only targets whose native format *is* the canonical `SKILL.md` (verbatim
/// or near enough that unknown frontmatter keys are simply ignored by the
/// consumer) can symlink. Anything that transforms the format (Cursor
/// `.mdc`) or shares a file with user-authored content (Copilot, MCP JSON)
/// must compile — a symlink there would hand the user's file over to
/// whatever the skill last contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStrategy {
    Symlink,
    Compile,
}

/// One thing `skx` writes into a target agent's config tree.
///
/// The three variants exist because targets differ in how much of the
/// destination file they own:
/// - Antigravity/Claude Code/Cursor each get their own file skx fully owns.
/// - GitHub Copilot's `copilot-instructions.md` is shared with the user;
///   skx owns only the region between its start/end markers.
/// - MCP configs (`.vscode/mcp.json`, `goose/mcp.json`) are shared
///   structured files; skx owns only specific keys.
///
/// Modeling `Region`/`MergeJson` distinctly (instead of "just write files")
/// is what makes it possible to sync without clobbering content the user
/// authored in the same file.
#[derive(Debug, Clone, PartialEq)]
pub enum Artifact {
    /// skx owns this file outright — safe to overwrite wholesale, or to
    /// replace with a symlink when [`LinkStrategy::Symlink`] applies.
    OwnedFile { path: PathBuf, contents: String },
    /// skx owns only the region between
    /// `<!-- skx:start {marker} -->` and `<!-- skx:end {marker} -->`
    /// inside a file the user also edits directly.
    Region {
        path: PathBuf,
        marker: String,
        contents: String,
    },
    /// skx owns only the value at `pointer` (an RFC 6901 JSON pointer, e.g.
    /// `/mcpServers/rust-analyzer-mcp`) inside a structured config file the
    /// user also edits directly.
    MergeJson {
        path: PathBuf,
        pointer: String,
        value: JsonValue,
    },
    /// skx owns this whole directory, mirrored from `source` in the cache.
    ///
    /// The Agent Skills spec defines a skill as a *directory* — `SKILL.md`
    /// plus optional `scripts/`, `references/` and `assets/` that the body
    /// references by relative path. Modelling only the `SKILL.md` left those
    /// siblings behind: it happened to work when the link landed back beside
    /// the originals, and broke the moment a skill was exported or synced to
    /// any other target root, with the file references silently dangling.
    ///
    /// Only [`LinkStrategy::Symlink`] targets use this. A compiling target
    /// transforms the skill into its own format and has no directory to
    /// mirror.
    OwnedDir { path: PathBuf, source: PathBuf },
}

impl Artifact {
    /// The filesystem path this artifact lives at, regardless of variant.
    pub fn path(&self) -> &Path {
        match self {
            Artifact::OwnedFile { path, .. } => path,
            Artifact::Region { path, .. } => path,
            Artifact::MergeJson { path, .. } => path,
            Artifact::OwnedDir { path, .. } => path,
        }
    }
}

/// The full set of artifacts an adapter produces for a single skill.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompiledOutput {
    pub artifacts: Vec<Artifact>,
}

/// Where a compile call should write, and at what scope.
///
/// Bundling `root`/`home`/`scope` here (rather than passing `global: bool`
/// separately from `compile`) means an adapter can resolve its own output
/// paths and render content in the same call — no separate
/// `resolve_paths` step that could disagree with what `compile` produced.
pub struct CompileCtx<'a> {
    /// Workspace root, used when `scope` is [`crate::Scope::Local`].
    pub root: &'a Path,
    /// The user's home directory. Always available even in local scope,
    /// since some targets (e.g. Claude Code) read from both.
    pub home: &'a Path,
    /// Which scope's *output* paths to resolve.
    pub scope: crate::Scope,
    /// The skill's canonical directory in the cache.
    ///
    /// Passed explicitly rather than derived from `scope`, because the two
    /// genuinely differ: `skx export` redirects output into a throwaway
    /// directory by setting `root`/`scope`, while the skill itself still
    /// lives wherever it was installed. Deriving the source from `scope`
    /// made export look for a global skill in a local cache that had never
    /// existed.
    pub cache: &'a Path,
}
