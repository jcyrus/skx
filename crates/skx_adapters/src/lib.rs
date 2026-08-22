//! Compiles canonical [`skx_core::Skill`]s into target-agent-specific formats
//! (Antigravity, Claude Code, Cursor, GitHub Copilot, MCP configs).
//!
//! The [`SkillAdapter`] trait and the per-target implementations live here;
//! the generic write primitives they produce ([`skx_core::Artifact`],
//! [`skx_core::CompiledOutput`], [`skx_core::LinkStrategy`],
//! [`skx_core::CompileCtx`]) live in `skx_core` instead, since they carry no
//! agent-specific knowledge — that's what lets `skx_core::sync` host a
//! single write engine for every adapter's output.

use skx_core::Skill;
pub use skx_core::{Artifact, CompileCtx, CompiledOutput, LinkStrategy};
use thiserror::Error;

mod antigravity;
mod claude_code;
mod copilot;
mod cursor;
mod mcp;

pub use antigravity::AntigravityAdapter;
pub use claude_code::ClaudeCodeAdapter;
pub use copilot::CopilotAdapter;
pub use cursor::CursorAdapter;
pub use mcp::McpAdapter;

/// Target ids that adapters read out of [`skx_core::Frontmatter::targets`].
/// Pass this to [`skx_core::unknown_targets`] to flag typos like
/// `claude-code` vs `claude_code`. Deliberately excludes `"mcp"`: MCP
/// dependencies are driven by the top-level `mcp_dependencies` list, not a
/// `targets.mcp` block, so that id never appears as a targets key.
pub const KNOWN_TARGET_IDS: &[&str] = &["antigravity", "claude_code", "cursor", "copilot"];

/// One adapter per target this build of skx knows how to compile to.
pub fn default_adapters() -> Vec<Box<dyn SkillAdapter>> {
    vec![
        Box::new(AntigravityAdapter),
        Box::new(ClaudeCodeAdapter),
        Box::new(CursorAdapter),
        Box::new(CopilotAdapter),
        Box::new(McpAdapter),
    ]
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("adapter {target} does not support skill {skill}: {reason}")]
    Unsupported {
        target: &'static str,
        skill: String,
        reason: String,
    },

    #[error("failed to render output for {target}: {source}")]
    Render {
        target: &'static str,
        #[source]
        source: anyhow::Error,
    },
}

pub type Result<T> = std::result::Result<T, AdapterError>;

/// A translator from the canonical [`Skill`] format into one target agent's
/// native dialect.
pub trait SkillAdapter {
    /// Stable identifier for this target, e.g. `"claude_code"`. Must match
    /// the key adapters look up in [`skx_core::Frontmatter::targets`].
    fn target_name(&self) -> &'static str;

    /// Whether this target's output can be symlinked to the canonical
    /// skill or must always be written out. See [`LinkStrategy`].
    fn link_strategy(&self) -> LinkStrategy;

    /// Compile a skill into the artifact(s) this target expects, with
    /// destination paths already resolved against `ctx`.
    fn compile(&self, skill: &Skill, ctx: &CompileCtx) -> Result<CompiledOutput>;
}
