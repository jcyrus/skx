use skx_core::{Scope, Skill};

use crate::{Artifact, CompileCtx, CompiledOutput, LinkStrategy, Result, SkillAdapter};

/// Compiles to Antigravity's own `SKILL.md` format, which *is* the
/// canonical format skx already stores — so the output can always be a
/// symlink back to the source rather than a copy.
///
/// A skill only targets Antigravity if its frontmatter declares a
/// `targets.antigravity` block at all; there's no separate `enabled` flag
/// to check (unlike Claude Code), since the block's presence *is* the
/// opt-in.
pub struct AntigravityAdapter;

impl SkillAdapter for AntigravityAdapter {
    fn target_name(&self) -> &'static str {
        "antigravity"
    }

    fn link_strategy(&self) -> LinkStrategy {
        LinkStrategy::Symlink
    }

    fn compile(&self, skill: &Skill, ctx: &CompileCtx) -> Result<CompiledOutput> {
        if !skill.frontmatter.targets.contains_key("antigravity") {
            return Ok(CompiledOutput::default());
        }

        let contents =
            skx_core::render_skill(skill).map_err(|source| crate::AdapterError::Render {
                target: self.target_name(),
                source: source.into(),
            })?;

        let path = match ctx.scope {
            Scope::Local => ctx
                .root
                .join(".agents/skills")
                .join(skill.frontmatter.name.as_str())
                .join("SKILL.md"),
            Scope::Global => ctx
                .home
                .join(".gemini/config/skills")
                .join(skill.frontmatter.name.as_str())
                .join("SKILL.md"),
        };

        Ok(CompiledOutput {
            artifacts: vec![Artifact::OwnedFile { path, contents }],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use skx_core::parse_skill;

    use super::*;

    fn skill_with_antigravity() -> Skill {
        let src = "---\nname: t\ndescription: d\ntargets:\n  antigravity:\n    scope: workspace\n    auto_activate: true\n---\nbody\n";
        parse_skill(src, Path::new("SKILL.md")).unwrap()
    }

    fn skill_without_targets() -> Skill {
        let src = "---\nname: t\ndescription: d\n---\nbody\n";
        parse_skill(src, Path::new("SKILL.md")).unwrap()
    }

    #[test]
    fn skips_when_target_not_declared() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
        };
        let output = AntigravityAdapter
            .compile(&skill_without_targets(), &ctx)
            .unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn resolves_local_path() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
        };
        let output = AntigravityAdapter
            .compile(&skill_with_antigravity(), &ctx)
            .unwrap();
        assert_eq!(output.artifacts.len(), 1);
        assert_eq!(
            output.artifacts[0].path(),
            Path::new("/workspace/.agents/skills/t/SKILL.md")
        );
    }

    #[test]
    fn resolves_global_path() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Global,
        };
        let output = AntigravityAdapter
            .compile(&skill_with_antigravity(), &ctx)
            .unwrap();
        assert_eq!(
            output.artifacts[0].path(),
            Path::new("/home/user/.gemini/config/skills/t/SKILL.md")
        );
    }

    #[test]
    fn output_is_canonical_render() {
        let ctx = CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
        };
        let skill = skill_with_antigravity();
        let output = AntigravityAdapter.compile(&skill, &ctx).unwrap();
        let Artifact::OwnedFile { contents, .. } = &output.artifacts[0] else {
            panic!("expected an OwnedFile artifact");
        };
        assert_eq!(contents, &skx_core::render_skill(&skill).unwrap());
    }
}
