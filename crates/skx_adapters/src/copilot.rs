use serde::Deserialize;
use skx_core::{Scope, Skill};

use crate::{
    AdapterError, Artifact, CompileCtx, CompiledOutput, LinkStrategy, Result, SkillAdapter,
};

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct CopilotCfg {
    #[serde(default = "default_true")]
    enabled: bool,
}

/// Injects a skill's instructions into `.github/copilot-instructions.md`
/// inside `<!-- skx:start <name> -->` / `<!-- skx:end <name> -->` markers.
///
/// That file is hand-edited by the user too, so this is always
/// `Artifact::Region`, never a whole-file write — and always
/// `LinkStrategy::Compile`, since there is nothing to symlink to.
/// Copilot has no global instructions file, so `Scope::Global` is
/// unsupported.
pub struct CopilotAdapter;

impl SkillAdapter for CopilotAdapter {
    fn target_name(&self) -> &'static str {
        "copilot"
    }

    fn link_strategy(&self) -> LinkStrategy {
        LinkStrategy::Compile
    }

    fn compile(&self, skill: &Skill, ctx: &CompileCtx) -> Result<CompiledOutput> {
        let Some(raw) = skill.frontmatter.targets.get("copilot") else {
            return Ok(CompiledOutput::default());
        };
        let cfg: CopilotCfg =
            serde_yaml::from_value(raw.clone()).map_err(|source| AdapterError::Render {
                target: self.target_name(),
                source: source.into(),
            })?;
        if !cfg.enabled {
            return Ok(CompiledOutput::default());
        }

        if ctx.scope == Scope::Global {
            return Err(AdapterError::Unsupported {
                target: self.target_name(),
                skill: skill.frontmatter.name.to_string(),
                reason: "GitHub Copilot has no global instructions file".to_string(),
            });
        }

        let name = skill.frontmatter.name.as_str();
        let contents = format!("## {name}\n\n{}\n", skill.body.trim_end());

        Ok(CompiledOutput {
            artifacts: vec![Artifact::Region {
                path: ctx.root.join(".github/copilot-instructions.md"),
                marker: name.to_string(),
                contents,
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use skx_core::parse_skill;

    use super::*;

    fn ctx(scope: Scope) -> CompileCtx<'static> {
        CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope,
            cache: Path::new("/cache/t"),
        }
    }

    #[test]
    fn skips_when_target_not_declared() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = CopilotAdapter.compile(&skill, &ctx(Scope::Local)).unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn produces_a_marked_region() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\ntargets:\n  copilot:\n    enabled: true\n---\ninstructions here\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = CopilotAdapter.compile(&skill, &ctx(Scope::Local)).unwrap();
        let Artifact::Region {
            path,
            marker,
            contents,
        } = &output.artifacts[0]
        else {
            panic!("expected a Region artifact");
        };
        assert_eq!(
            path,
            Path::new("/workspace/.github/copilot-instructions.md")
        );
        assert_eq!(marker, "t");
        assert!(contents.contains("instructions here"));
    }

    #[test]
    fn skips_when_explicitly_disabled() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\ntargets:\n  copilot:\n    enabled: false\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = CopilotAdapter.compile(&skill, &ctx(Scope::Local)).unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn errors_on_global_scope() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\ntargets:\n  copilot:\n    enabled: true\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let err = CopilotAdapter
            .compile(&skill, &ctx(Scope::Global))
            .unwrap_err();
        assert!(matches!(err, AdapterError::Unsupported { .. }));
    }
}
