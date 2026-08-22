use serde::{Deserialize, Serialize};
use skx_core::{Scope, Skill};

use crate::{
    AdapterError, Artifact, CompileCtx, CompiledOutput, LinkStrategy, Result, SkillAdapter,
};

#[derive(Debug, Default, Deserialize)]
struct CursorCfg {
    #[serde(default)]
    glob: Option<String>,
}

#[derive(Debug, Serialize)]
struct CursorFrontmatter {
    description: String,
    globs: Vec<String>,
    #[serde(rename = "alwaysApply")]
    always_apply: bool,
}

/// Compiles into a Cursor `.mdc` rule. Cursor's rule schema
/// (`description`/`globs`/`alwaysApply`) is a real format transformation,
/// not a relabeled canonical `SKILL.md` — so this adapter must always
/// write out a translated file, never symlink.
pub struct CursorAdapter;

impl SkillAdapter for CursorAdapter {
    fn target_name(&self) -> &'static str {
        "cursor"
    }

    fn link_strategy(&self) -> LinkStrategy {
        LinkStrategy::Compile
    }

    fn compile(&self, skill: &Skill, ctx: &CompileCtx) -> Result<CompiledOutput> {
        let Some(raw) = skill.frontmatter.targets.get("cursor") else {
            return Ok(CompiledOutput::default());
        };
        let cfg: CursorCfg =
            serde_yaml::from_value(raw.clone()).map_err(|source| AdapterError::Render {
                target: self.target_name(),
                source: source.into(),
            })?;

        // Contract documented on `Frontmatter::triggers`: an explicit
        // per-target glob wins; otherwise translate the canonical triggers.
        let globs = match cfg.glob {
            Some(glob) => vec![glob],
            None => skill.frontmatter.triggers.clone(),
        };
        if globs.is_empty() {
            return Err(AdapterError::Unsupported {
                target: self.target_name(),
                skill: skill.frontmatter.name.to_string(),
                reason: "no cursor glob override and no canonical triggers to translate"
                    .to_string(),
            });
        }

        let frontmatter = CursorFrontmatter {
            description: skill.frontmatter.description.clone(),
            globs,
            always_apply: false,
        };
        let yaml = serde_yaml::to_string(&frontmatter).map_err(|source| AdapterError::Render {
            target: self.target_name(),
            source: source.into(),
        })?;
        let contents = format!("---\n{yaml}---\n\n{}", skill.body);

        let path = match ctx.scope {
            Scope::Local => ctx
                .root
                .join(".cursor/rules")
                .join(format!("{}.mdc", skill.frontmatter.name.as_str())),
            Scope::Global => ctx
                .home
                .join(".config/cursor/rules")
                .join(format!("{}.mdc", skill.frontmatter.name.as_str())),
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

    fn skill_with(target_block: &str, triggers: &str) -> Skill {
        let src =
            format!("---\nname: t\ndescription: d\n{triggers}targets:\n{target_block}---\nbody\n");
        parse_skill(&src, Path::new("SKILL.md")).unwrap()
    }

    fn ctx() -> CompileCtx<'static> {
        CompileCtx {
            root: Path::new("/workspace"),
            home: Path::new("/home/user"),
            scope: Scope::Local,
        }
    }

    #[test]
    fn skips_when_target_not_declared() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = CursorAdapter.compile(&skill, &ctx()).unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn explicit_glob_overrides_triggers() {
        let skill = skill_with(
            "  cursor:\n    glob: \"**/*.rs\"\n",
            "triggers:\n  - \"*.py\"\n",
        );
        let output = CursorAdapter.compile(&skill, &ctx()).unwrap();
        let Artifact::OwnedFile { contents, path } = &output.artifacts[0] else {
            panic!("expected an OwnedFile artifact");
        };
        assert_eq!(path, Path::new("/workspace/.cursor/rules/t.mdc"));
        assert!(contents.contains("**/*.rs"), "contents: {contents}");
        assert!(!contents.contains("*.py"));
    }

    #[test]
    fn falls_back_to_canonical_triggers() {
        let skill = skill_with(
            "  cursor: {}\n",
            "triggers:\n  - \"*.rs\"\n  - Cargo.toml\n",
        );
        let output = CursorAdapter.compile(&skill, &ctx()).unwrap();
        let Artifact::OwnedFile { contents, .. } = &output.artifacts[0] else {
            panic!("expected an OwnedFile artifact");
        };
        assert!(contents.contains("*.rs"));
        assert!(contents.contains("Cargo.toml"));
    }

    #[test]
    fn errors_when_no_glob_and_no_triggers() {
        let skill = skill_with("  cursor: {}\n", "");
        let err = CursorAdapter.compile(&skill, &ctx()).unwrap_err();
        assert!(matches!(err, AdapterError::Unsupported { .. }));
    }
}
