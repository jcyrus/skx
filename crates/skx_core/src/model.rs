use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::SkillError;

/// A fully parsed skill: its frontmatter metadata plus the markdown instruction body.
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub frontmatter: Frontmatter,
    /// Markdown content following the `---` frontmatter delimiters.
    pub body: String,
    /// Absolute path to the source `SKILL.md`, if loaded from disk.
    pub source_path: Option<PathBuf>,
}

/// Canonical YAML frontmatter schema shared by every `SKILL.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    pub name: SkillName,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    /// Canonical, adapter-agnostic activation triggers (glob patterns).
    ///
    /// Contract for adapters: if a target's own config block (e.g. Cursor's
    /// `glob`) is present, the adapter should prefer that override; only
    /// fall back to translating these canonical triggers when the target
    /// doesn't declare its own.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Per-agent target configuration, keyed by adapter id (e.g.
    /// `"claude_code"`, `"cursor"`). Deliberately opaque: `skx_core` does
    /// not know which target ids are valid or what shape their config
    /// takes — each `SkillAdapter` implementation (in `skx_adapters`)
    /// deserializes its own slice of this map with `serde_yaml::from_value`.
    /// Use a `BTreeMap` (not `HashMap`) so iteration order — and therefore
    /// re-rendered YAML — is deterministic across runs; drift detection
    /// depends on that.
    #[serde(default)]
    pub targets: BTreeMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub mcp_dependencies: Vec<McpDependency>,

    // ── Agent Skills spec fields ────────────────────────────────────────
    // Everything below is optional in the spec and skipped when absent, so
    // a skill that never declared them re-renders byte-identically to what
    // it was before these fields existed. That matters: drift detection
    // hashes the rendered file, so a formatting-only change here would
    // flag every synced artifact in every workspace as user-modified.
    /// License name, or a reference to a bundled license file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Environment requirements — intended product, system packages,
    /// network access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    /// Space-separated list of pre-approved tools. Experimental in the
    /// spec, and hyphenated there rather than snake_case.
    #[serde(
        default,
        rename = "allowed-tools",
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_tools: Option<String>,
    /// Arbitrary string→string metadata. The spec's own example puts
    /// `author` here, which makes this the canonical home for attribution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,

    /// Every frontmatter key `skx` doesn't model, preserved verbatim.
    ///
    /// Without this, `serde` silently drops unknown keys and `render_skill`
    /// writes back only what it knew — so installing a skill would destroy
    /// any field a future spec version, or an individual skill author,
    /// added. `skx` symlinks its rendered copy over the original, so that
    /// loss is not recoverable.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl Frontmatter {
    /// Who wrote this skill, per the spec's `metadata.author` convention.
    pub fn author(&self) -> Option<&str> {
        self.metadata.get("author").map(String::as_str)
    }

    /// The skill's effective version.
    ///
    /// `skx` models `version` at the top level, but the spec's example
    /// carries it as `metadata.version`. A skill written to the spec would
    /// otherwise be silently renumbered to the `0.1.0` default, so the
    /// metadata value wins whenever no explicit top-level one was given.
    pub fn effective_version(&self) -> &str {
        if self.version == default_version()
            && let Some(from_metadata) = self.metadata.get("version")
        {
            return from_metadata;
        }
        &self.version
    }

    /// Rough context cost of loading this skill, in tokens.
    ///
    /// Four characters per token is the usual English approximation; it is
    /// deliberately not exact, because the point of the number is relative
    /// ranking ("this skill costs 3x that one") rather than billing.
    pub fn approx_tokens(&self, body: &str) -> usize {
        (self.description.len() + body.len()) / 4
    }
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// Returns the keys in `frontmatter.targets` that don't match any id in
/// `known`. Core has no adapter registry of its own, so callers that do
/// (typically `skx_cli`, which links every registered `SkillAdapter`) pass
/// their known ids here. Catches typos like `claude-code` vs `claude_code`
/// that would otherwise parse silently and simply never compile for that
/// target.
pub fn unknown_targets(frontmatter: &Frontmatter, known: &[&str]) -> Vec<String> {
    frontmatter
        .targets
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .cloned()
        .collect()
}

/// A validated skill identifier: lowercase ASCII letters, digits, and
/// internal hyphens only. Used directly as a path segment by every adapter
/// (`~/.claude/skills/<name>/`, `.cursor/rules/<name>.mdc`, ...), so it must
/// never contain `/`, `..`, or other path-traversal-capable characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SkillName(String);

impl SkillName {
    pub fn parse(raw: &str) -> Result<Self, SkillError> {
        if raw.is_empty() {
            return Err(SkillError::InvalidName {
                name: raw.to_string(),
                reason: "must not be empty".to_string(),
            });
        }
        if raw.len() > 128 {
            return Err(SkillError::InvalidName {
                name: raw.to_string(),
                reason: "must be 128 characters or fewer".to_string(),
            });
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(SkillError::InvalidName {
                name: raw.to_string(),
                reason: "must contain only lowercase ascii letters, digits, and hyphens"
                    .to_string(),
            });
        }
        if raw.starts_with('-') || raw.ends_with('-') {
            return Err(SkillError::InvalidName {
                name: raw.to_string(),
                reason: "must not start or end with a hyphen".to_string(),
            });
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SkillName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        SkillName::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl PartialEq<str> for SkillName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for SkillName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A Model Context Protocol server this skill depends on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpDependency {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// `BTreeMap`, not `HashMap`: env vars round-trip through
    /// `render_skill` and must serialize in a stable order.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Where a skill is installed relative to the current workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    Local,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_name() {
        let err = SkillName::parse("../../../tmp/pwned").unwrap_err();
        assert!(matches!(err, SkillError::InvalidName { .. }));
    }

    #[test]
    fn rejects_empty_and_hyphen_edges() {
        assert!(SkillName::parse("").is_err());
        assert!(SkillName::parse("-leading").is_err());
        assert!(SkillName::parse("trailing-").is_err());
    }

    #[test]
    fn rejects_uppercase_and_dots() {
        assert!(SkillName::parse("Rust-Expert").is_err());
        assert!(SkillName::parse("rust.expert").is_err());
    }

    #[test]
    fn accepts_kebab_case_name() {
        assert!(SkillName::parse("rust-systems-expert").is_ok());
    }

    #[test]
    fn unknown_targets_flags_typos() {
        let mut targets = BTreeMap::new();
        targets.insert("claude-code".to_string(), serde_yaml::Value::Null);
        targets.insert("cursor".to_string(), serde_yaml::Value::Null);
        let frontmatter = Frontmatter {
            name: SkillName::parse("t").unwrap(),
            description: "d".to_string(),
            version: default_version(),
            triggers: vec![],
            targets,
            mcp_dependencies: vec![],
            license: None,
            compatibility: None,
            allowed_tools: None,
            metadata: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        let unknown = unknown_targets(&frontmatter, &["claude_code", "cursor"]);
        assert_eq!(unknown, vec!["claude-code".to_string()]);
    }
}
