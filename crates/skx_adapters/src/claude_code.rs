use serde::Deserialize;
use skx_core::{Scope, Skill};

use crate::{
    AdapterError, Artifact, CompileCtx, CompiledOutput, LinkStrategy, Result, SkillAdapter,
};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeCfg {
    #[serde(default = "default_true")]
    enabled: bool,
}

/// Compiles to Claude Code's own skill format. Claude Code's `SKILL.md`
/// parser only reads `name`/`description` and ignores frontmatter keys it
/// doesn't recognize, so this is close enough to canonical `SKILL.md` that
/// the output can always be a symlink rather than a copy — unlike Cursor,
/// there's no real format transformation here, just a different
/// destination path. (An earlier version of this adapter trimmed the
/// frontmatter down to just `name`/`description` before writing, which was
/// wrong: trimming *is* a transformation, and transformed output can never
/// be a valid symlink target — a symlink mirrors the source byte-for-byte
/// by construction. If Claude Code ever needs a real transformation, this
/// adapter must switch to `LinkStrategy::Compile`.)
pub struct ClaudeCodeAdapter;

impl SkillAdapter for ClaudeCodeAdapter {
    fn target_name(&self) -> &'static str {
        "claude_code"
    }

    fn link_strategy(&self) -> LinkStrategy {
        LinkStrategy::Symlink
    }

    fn compile(&self, skill: &Skill, ctx: &CompileCtx) -> Result<CompiledOutput> {
        let Some(raw) = skill.frontmatter.targets.get("claude_code") else {
            return Ok(CompiledOutput::default());
        };
        let cfg: ClaudeCodeCfg =
            serde_yaml::from_value(raw.clone()).map_err(|source| AdapterError::Render {
                target: self.target_name(),
                source: source.into(),
            })?;
        if !cfg.enabled {
            return Ok(CompiledOutput::default());
        }

        // The whole skill directory is linked, not just `SKILL.md`, so any
        // `scripts/`, `references/` or `assets/` the body refers to by
        // relative path travel with it.
        let path = match ctx.scope {
            Scope::Local => ctx.root.join(".claude/skills"),
            Scope::Global => ctx.home.join(".claude/skills"),
        }
        .join(skill.frontmatter.name.as_str());

        Ok(CompiledOutput {
            artifacts: vec![Artifact::OwnedDir {
                path,
                source: ctx.cache.to_path_buf(),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use skx_core::parse_skill;

    use super::*;

    fn skill_with(target_block: &str) -> Skill {
        let src = format!("---\nname: t\ndescription: d\ntargets:\n{target_block}---\nbody\n");
        parse_skill(&src, Path::new("SKILL.md")).unwrap()
    }

    #[test]
    fn skips_when_target_not_declared() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
            cache: Path::new("/cache/t"),
        };
        let skill = parse_skill(
            "---\nname: t\ndescription: d\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = ClaudeCodeAdapter.compile(&skill, &ctx).unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn skips_when_explicitly_disabled() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
            cache: Path::new("/cache/t"),
        };
        let skill = skill_with("  claude_code:\n    enabled: false\n");
        let output = ClaudeCodeAdapter.compile(&skill, &ctx).unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn output_is_canonical_render() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
            cache: Path::new("/cache/t"),
        };
        let skill = skill_with("  claude_code:\n    enabled: true\n");
        let output = ClaudeCodeAdapter.compile(&skill, &ctx).unwrap();
        // The linked unit is the skill *directory*, so anything the skill
        // ships beside `SKILL.md` travels with it.
        let Artifact::OwnedDir { path, source } = &output.artifacts[0] else {
            panic!("expected an OwnedDir artifact");
        };
        assert_eq!(path, Path::new("/workspace/.claude/skills/t"));
        // The adapter must use `ctx.cache` verbatim rather than
        // re-deriving it from `scope`: export deliberately sets a different
        // output scope than the one the skill is actually installed at.
        assert_eq!(source, ctx.cache);
    }

    #[test]
    fn resolves_global_path() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Global,
            cache: Path::new("/cache/t"),
        };
        let skill = skill_with("  claude_code:\n    enabled: true\n");
        let output = ClaudeCodeAdapter.compile(&skill, &ctx).unwrap();
        assert_eq!(
            output.artifacts[0].path(),
            Path::new("/home/user/.claude/skills/t")
        );
    }
}
