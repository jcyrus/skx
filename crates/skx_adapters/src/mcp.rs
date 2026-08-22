use serde_json::json;
use skx_core::{Scope, Skill};

use crate::{Artifact, CompileCtx, CompiledOutput, LinkStrategy, Result, SkillAdapter};

/// Merges each of a skill's `mcp_dependencies` into the target runtime's
/// MCP config as its own JSON-pointer key under `/mcpServers`.
///
/// Unlike Antigravity/Claude Code/Cursor/Copilot, this isn't gated on a
/// `targets.*` block — the spec's canonical example has no
/// `targets.mcp` entry; `mcp_dependencies` is a top-level list that
/// applies wherever an MCP-capable runtime is configured. One
/// `Artifact::MergeJson` per dependency, never a whole-file write, since
/// the config is shared with entries from other skills and tools the user
/// configured directly.
pub struct McpAdapter;

impl SkillAdapter for McpAdapter {
    fn target_name(&self) -> &'static str {
        "mcp"
    }

    fn link_strategy(&self) -> LinkStrategy {
        LinkStrategy::Compile
    }

    fn compile(&self, skill: &Skill, ctx: &CompileCtx) -> Result<CompiledOutput> {
        if skill.frontmatter.mcp_dependencies.is_empty() {
            return Ok(CompiledOutput::default());
        }

        let path = match ctx.scope {
            Scope::Local => ctx.root.join(".vscode/mcp.json"),
            Scope::Global => ctx.home.join(".config/goose/mcp.json"),
        };

        let artifacts = skill
            .frontmatter
            .mcp_dependencies
            .iter()
            .map(|dep| {
                let mut value = json!({
                    "command": dep.command,
                    "args": dep.args,
                });
                if !dep.env.is_empty() {
                    value["env"] = json!(dep.env);
                }
                Artifact::MergeJson {
                    path: path.clone(),
                    pointer: format!("/mcpServers/{}", dep.name),
                    value,
                }
            })
            .collect();

        Ok(CompiledOutput { artifacts })
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
        }
    }

    #[test]
    fn skips_when_no_dependencies() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = McpAdapter.compile(&skill, &ctx(Scope::Local)).unwrap();
        assert!(output.artifacts.is_empty());
    }

    #[test]
    fn produces_one_merge_per_dependency() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\nmcp_dependencies:\n  - name: rust-analyzer-mcp\n    command: rust-analyzer-mcp\n    args: [\"--stdio\"]\n  - name: other\n    command: other-cmd\n    env:\n      KEY: value\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = McpAdapter.compile(&skill, &ctx(Scope::Local)).unwrap();
        assert_eq!(output.artifacts.len(), 2);

        let Artifact::MergeJson {
            path,
            pointer,
            value,
        } = &output.artifacts[0]
        else {
            panic!("expected a MergeJson artifact");
        };
        assert_eq!(path, Path::new("/workspace/.vscode/mcp.json"));
        assert_eq!(pointer, "/mcpServers/rust-analyzer-mcp");
        assert_eq!(value["command"], "rust-analyzer-mcp");
        assert_eq!(value["args"][0], "--stdio");
        assert!(value.get("env").is_none());

        let Artifact::MergeJson { value, .. } = &output.artifacts[1] else {
            panic!("expected a MergeJson artifact");
        };
        assert_eq!(value["env"]["KEY"], "value");
    }

    #[test]
    fn resolves_global_path() {
        let skill = parse_skill(
            "---\nname: t\ndescription: d\nmcp_dependencies:\n  - name: m\n    command: c\n---\nbody\n",
            Path::new("SKILL.md"),
        )
        .unwrap();
        let output = McpAdapter.compile(&skill, &ctx(Scope::Global)).unwrap();
        assert_eq!(
            output.artifacts[0].path(),
            Path::new("/home/user/.config/goose/mcp.json")
        );
    }
}
