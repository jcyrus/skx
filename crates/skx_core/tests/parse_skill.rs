use std::path::Path;

use serde::Deserialize;
use skx_core::{discover_skills, load_skill, parse_skill, render_skill, unknown_targets};

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

// Mirrors what a concrete `SkillAdapter` in `skx_adapters` would deserialize
// out of its own slice of the opaque `targets` map.
#[derive(Debug, Deserialize)]
struct AntigravityCfg {
    scope: String,
    auto_activate: bool,
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeCfg {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct CursorCfg {
    glob: String,
}

#[test]
fn parses_real_skill_md_fixture() {
    let path = fixture_path("rust-systems-expert/SKILL.md");
    let skill = load_skill(&path).expect("fixture should parse");

    assert_eq!(skill.frontmatter.name, "rust-systems-expert");
    assert_eq!(skill.frontmatter.version, "1.0.0");
    assert_eq!(
        skill.frontmatter.triggers,
        vec!["*.rs".to_string(), "Cargo.toml".to_string()]
    );

    let antigravity: AntigravityCfg = serde_yaml::from_value(
        skill
            .frontmatter
            .targets
            .get("antigravity")
            .cloned()
            .expect("antigravity target present"),
    )
    .expect("antigravity target should deserialize");
    assert_eq!(antigravity.scope, "workspace");
    assert!(antigravity.auto_activate);

    let claude_code: ClaudeCodeCfg = serde_yaml::from_value(
        skill
            .frontmatter
            .targets
            .get("claude_code")
            .cloned()
            .expect("claude_code target present"),
    )
    .expect("claude_code target should deserialize");
    assert!(claude_code.enabled);

    let cursor: CursorCfg = serde_yaml::from_value(
        skill
            .frontmatter
            .targets
            .get("cursor")
            .cloned()
            .expect("cursor target present"),
    )
    .expect("cursor target should deserialize");
    assert_eq!(cursor.glob, "**/*.rs");

    assert_eq!(skill.frontmatter.mcp_dependencies.len(), 1);
    let dep = &skill.frontmatter.mcp_dependencies[0];
    assert_eq!(dep.name, "rust-analyzer-mcp");
    assert_eq!(dep.command, "rust-analyzer-mcp");
    assert_eq!(dep.args, vec!["--stdio".to_string()]);

    assert!(
        skill
            .body
            .contains("# Rust Systems Engineering Instructions")
    );
    assert!(skill.body.contains("zero-cost abstractions"));
    assert_eq!(skill.source_path.as_deref(), Some(path.as_path()));

    // Every declared target matches a known adapter id — no typos.
    let known = ["antigravity", "claude_code", "cursor", "copilot"];
    assert!(unknown_targets(&skill.frontmatter, &known).is_empty());
}

#[test]
fn round_trips_through_render() {
    let path = fixture_path("rust-systems-expert/SKILL.md");
    let skill = load_skill(&path).expect("fixture should parse");

    let rendered = render_skill(&skill).expect("skill should render");
    let reparsed = parse_skill(&rendered, &path).expect("rendered output should reparse");

    assert_eq!(reparsed.frontmatter, skill.frontmatter);
    assert_eq!(reparsed.body.trim_end(), skill.body.trim_end());
}

#[test]
fn render_is_byte_stable_across_repeated_round_trips() {
    // Regression test for non-deterministic map ordering: previously
    // `targets`/`env` were `HashMap`s, so re-rendering the same skill could
    // reorder its own YAML on every run, which would make drift detection
    // report phantom diffs forever. `BTreeMap` fixes this; assert it stays
    // fixed by round-tripping several times and comparing bytes.
    let src = "---\nname: t\ndescription: d\ntargets:\n  cursor:\n    glob: \"**/*.rs\"\n  antigravity:\n    scope: workspace\nmcp_dependencies:\n  - name: m\n    command: c\n    env:\n      ALPHA: '1'\n      BRAVO: '2'\n      CHARLIE: '3'\n      DELTA: '4'\n      ECHO: '5'\n      FOXTROT: '6'\n---\nbody\n";
    let path = Path::new("SKILL.md");

    let first = render_skill(&parse_skill(src, path).unwrap()).unwrap();
    for _ in 0..10 {
        let again = render_skill(&parse_skill(src, path).unwrap()).unwrap();
        assert_eq!(again, first, "render output must be byte-stable");
    }
}

#[test]
fn discover_skills_finds_fixture() {
    let root = fixture_path("");
    let found = discover_skills(&root).expect("walk should succeed");
    assert!(
        found
            .iter()
            .any(|p| p.ends_with("rust-systems-expert/SKILL.md")),
        "expected to discover the rust-systems-expert fixture, found: {found:?}"
    );
}

const SPEC_SKILL: &str = "---\n\
name: pdf-processing\n\
description: Extract PDF text, fill forms, merge files.\n\
license: Apache-2.0\n\
compatibility: Requires python3 and network access\n\
allowed-tools: Bash Read Write\n\
metadata:\n  \
  author: example-org\n  \
  version: '1.0'\n\
vendor-specific-field: keep me\n\
---\n\n\
# PDF Processing\n";

fn parse(raw: &str) -> skx_core::Skill {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("SKILL.md");
    std::fs::write(&path, raw).unwrap();
    skx_core::load_skill(&path).unwrap()
}

#[test]
fn spec_optional_fields_are_parsed() {
    let skill = parse(SPEC_SKILL);
    let fm = &skill.frontmatter;
    assert_eq!(fm.license.as_deref(), Some("Apache-2.0"));
    assert_eq!(
        fm.compatibility.as_deref(),
        Some("Requires python3 and network access")
    );
    assert_eq!(fm.allowed_tools.as_deref(), Some("Bash Read Write"));
    assert_eq!(fm.author(), Some("example-org"));
}

#[test]
fn unknown_frontmatter_keys_survive_a_round_trip() {
    // `skx` symlinks its rendered copy over the original, so anything it
    // drops here is destroyed rather than merely ignored.
    let skill = parse(SPEC_SKILL);
    assert!(
        skill
            .frontmatter
            .extra
            .contains_key("vendor-specific-field")
    );

    let rendered = skx_core::render_skill(&skill).unwrap();
    assert!(rendered.contains("vendor-specific-field: keep me"));
    assert!(rendered.contains("license: Apache-2.0"));
    assert!(rendered.contains("allowed-tools: Bash Read Write"));
    assert!(rendered.contains("author: example-org"));

    // ...and re-parsing the rendered form is a fixed point.
    assert_eq!(parse(&rendered).frontmatter, skill.frontmatter);
}

#[test]
fn a_spec_version_in_metadata_is_not_silently_reset_to_the_default() {
    let skill = parse(SPEC_SKILL);
    assert_eq!(skill.frontmatter.effective_version(), "1.0");
}

#[test]
fn an_explicit_top_level_version_wins_over_metadata() {
    let raw =
        "---\nname: x\ndescription: d\nversion: 2.5.0\nmetadata:\n  version: '1.0'\n---\nbody\n";
    assert_eq!(parse(raw).frontmatter.effective_version(), "2.5.0");
}

#[test]
fn skills_without_the_optional_fields_render_exactly_as_before() {
    // Drift detection hashes the rendered file, so adding fields to the
    // model must not change the bytes written for a skill that doesn't use
    // them — otherwise every synced artifact everywhere flags as modified.
    let raw = "---\nname: minimal\ndescription: a minimal skill\nversion: 0.1.0\ntriggers: []\ntargets: {}\nmcp_dependencies: []\n---\n\nbody text\n";
    let rendered = skx_core::render_skill(&parse(raw)).unwrap();
    assert_eq!(rendered, raw);
}

#[test]
fn approx_tokens_scales_with_content() {
    let small = parse("---\nname: a\ndescription: d\n---\nshort\n");
    let large = parse(&format!(
        "---\nname: b\ndescription: d\n---\n{}\n",
        "x".repeat(4000)
    ));
    assert!(small.frontmatter.approx_tokens(&small.body) < 10);
    assert!((950..=1010).contains(&large.frontmatter.approx_tokens(&large.body)));
}
