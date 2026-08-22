use std::path::Path;

use skx_adapters::{Artifact, CompileCtx, KNOWN_TARGET_IDS, default_adapters};
use skx_core::{Scope, parse_skill, unknown_targets};

const SAMPLE_SKILL: &str = "---\nname: rust-systems-expert\ndescription: Deep systems architectural conventions, memory layout, and concurrency patterns\nversion: 1.0.0\ntriggers:\n  - \"*.rs\"\n  - \"Cargo.toml\"\ntargets:\n  antigravity:\n    scope: workspace\n    auto_activate: true\n  claude_code:\n    enabled: true\n  cursor:\n    glob: \"**/*.rs\"\nmcp_dependencies:\n  - name: rust-analyzer-mcp\n    command: rust-analyzer-mcp\n    args: [\"--stdio\"]\n---\n\n# Rust Systems Engineering Instructions\n\n- Prefer zero-cost abstractions.\n";

#[test]
fn every_registered_adapter_compiles_the_sample_skill_without_error() {
    let skill = parse_skill(SAMPLE_SKILL, Path::new("SKILL.md")).expect("sample should parse");

    assert!(
        unknown_targets(&skill.frontmatter, KNOWN_TARGET_IDS).is_empty(),
        "sample skill's targets should all match a known adapter id"
    );

    let ctx = CompileCtx {
        root: Path::new("/workspace"),
        home: Path::new("/home/user"),
        scope: Scope::Local,
    };

    let mut artifact_counts = Vec::new();
    for adapter in default_adapters() {
        let output = adapter
            .compile(&skill, &ctx)
            .unwrap_or_else(|e| panic!("{} adapter failed: {e}", adapter.target_name()));
        artifact_counts.push((adapter.target_name(), output.artifacts.len()));
    }

    // antigravity, claude_code, cursor, and mcp all declare (or imply) this
    // target; copilot doesn't appear in `targets`, so it should skip.
    assert_eq!(
        artifact_counts,
        vec![
            ("antigravity", 1),
            ("claude_code", 1),
            ("cursor", 1),
            ("copilot", 0),
            ("mcp", 1),
        ]
    );
}

#[test]
fn mcp_artifact_merges_into_vscode_config_at_expected_pointer() {
    let skill = parse_skill(SAMPLE_SKILL, Path::new("SKILL.md")).unwrap();
    let ctx = CompileCtx {
        root: Path::new("/workspace"),
        home: Path::new("/home/user"),
        scope: Scope::Local,
    };

    let mcp = default_adapters()
        .into_iter()
        .find(|a| a.target_name() == "mcp")
        .unwrap();
    let output = mcp.compile(&skill, &ctx).unwrap();

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
}
