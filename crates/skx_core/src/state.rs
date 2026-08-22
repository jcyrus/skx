//! Drift detection state: a fingerprint of every artifact `skx` has written,
//! persisted at `.skx/state.toml`.
//!
//! Without this, `skx audit` can only tell you "the file doesn't match what
//! we'd generate right now" — which is indistinguishable from "the user
//! edited it" vs "the skill changed upstream" vs "nothing's wrong, we just
//! haven't run sync since". Recording the hash we wrote lets audit tell
//! those three states apart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::artifact::Artifact;
use crate::error::{Result, SkillError};

/// Where the drift-detection state file lives, given a workspace root.
/// Shared by `skx_cli` and `skx_tui` so both resolve the same file.
pub fn state_path(root: &Path) -> PathBuf {
    root.join(".skx/state.toml")
}

/// The `(kind, sub_key)` an `ArtifactRecord` should use for `artifact`.
/// Shared by `skx_cli`'s sync/audit commands and `skx_tui`'s status matrix
/// so every caller computes the same state key for the same artifact —
/// duplicating this match once already caused records to collide (see
/// `sub_key` below); a second copy would only reopen that risk.
pub fn artifact_kind_and_sub_key(artifact: &Artifact) -> (ArtifactKind, Option<String>) {
    match artifact {
        Artifact::OwnedFile { .. } => (ArtifactKind::OwnedFile, None),
        Artifact::Region { marker, .. } => (ArtifactKind::Region, Some(marker.clone())),
        Artifact::MergeJson { pointer, .. } => (ArtifactKind::MergeJson, Some(pointer.clone())),
    }
}

/// Which write strategy produced an artifact. Mirrors the shape of
/// `skx_adapters::Artifact` without `skx_core` depending on that crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// skx owns the whole file outright.
    OwnedFile,
    /// skx owns a marked region inside a file the user also edits.
    Region,
    /// skx owns specific keys inside a structured config the user also edits.
    MergeJson,
}

/// A fingerprint of one artifact `skx` wrote for one skill/target pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub path: PathBuf,
    /// Disambiguates artifacts that share `path`: the JSON pointer for
    /// `MergeJson` (one skill can merge several MCP dependencies into the
    /// same `mcp.json`, each at its own pointer) or the region marker for
    /// `Region`. `None` for `OwnedFile`, where `path` alone is already
    /// unique. Records are keyed by `(path, sub_key)`, not `path` alone —
    /// without this, two `MergeJson` artifacts at the same path would
    /// silently overwrite each other in state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_key: Option<String>,
    pub skill: String,
    pub skill_version: String,
    pub target: String,
    pub kind: ArtifactKind,
    /// sha256 hex digest of the content skx wrote. For a plain `OwnedFile`
    /// write this is the whole file; for `Region`/`MergeJson` it's just the
    /// region/value text; for a symlinked `OwnedFile` (see
    /// `symlink_target`) it's the resolved content of the symlink's target
    /// at the moment sync created it — not a re-render, since re-rendering
    /// the canonical skill isn't guaranteed byte-identical to the original
    /// cache file (YAML key ordering, quoting style) and would otherwise
    /// manufacture phantom drift on every audit.
    pub content_hash: String,
    /// Set when this artifact was written as a symlink rather than a
    /// plain file. Audit checks the link itself (`read_link(path) ==
    /// symlink_target`) instead of re-hashing content for these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<PathBuf>,
}

impl ArtifactRecord {
    /// Whether `content` still matches what skx last wrote for this artifact.
    pub fn matches(&self, content: &[u8]) -> bool {
        self.content_hash == hash_content(content)
    }
}

/// Persisted at `.skx/state.toml`. Records every artifact skx has written so
/// drift can be detected without recompiling every skill on every run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub artifacts: Vec<ArtifactRecord>,
}

impl StateFile {
    /// Loads state from `path`, or returns an empty state if the file
    /// doesn't exist yet (first-ever `sync`).
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|source| SkillError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| SkillError::InvalidState {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).map_err(|source| SkillError::SerializeState {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SkillError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(path, raw).map_err(|source| SkillError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn record_for(&self, path: &Path, sub_key: Option<&str>) -> Option<&ArtifactRecord> {
        self.artifacts
            .iter()
            .find(|a| a.path == path && a.sub_key.as_deref() == sub_key)
    }

    /// Inserts `record`, replacing any existing record with the same
    /// `(path, sub_key)`.
    pub fn upsert(&mut self, record: ArtifactRecord) {
        match self
            .artifacts
            .iter_mut()
            .find(|a| a.path == record.path && a.sub_key == record.sub_key)
        {
            Some(existing) => *existing = record,
            None => self.artifacts.push(record),
        }
    }

    /// Removes every record belonging to `skill`, returning them.
    pub fn remove_skill(&mut self, skill: &str) -> Vec<ArtifactRecord> {
        let (removed, kept): (Vec<_>, Vec<_>) =
            self.artifacts.drain(..).partition(|a| a.skill == skill);
        self.artifacts = kept;
        removed
    }
}

/// sha256 hex digest of `content`, used as the fingerprint stored in
/// [`ArtifactRecord::content_hash`].
pub fn hash_content(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(path: &str) -> ArtifactRecord {
        ArtifactRecord {
            path: PathBuf::from(path),
            sub_key: None,
            skill: "rust-systems-expert".to_string(),
            skill_version: "1.0.0".to_string(),
            target: "claude_code".to_string(),
            kind: ArtifactKind::OwnedFile,
            content_hash: hash_content(b"hello"),
            symlink_target: None,
        }
    }

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash_content(b"hello"), hash_content(b"hello"));
        assert_ne!(hash_content(b"hello"), hash_content(b"world"));
    }

    #[test]
    fn record_matches_checks_hash() {
        let record = sample_record("/tmp/x/SKILL.md");
        assert!(record.matches(b"hello"));
        assert!(!record.matches(b"tampered"));
    }

    #[test]
    fn missing_state_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".skx/state.toml");
        let state = StateFile::load(&path).unwrap();
        assert!(state.artifacts.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".skx/state.toml");

        let mut state = StateFile::default();
        state.upsert(sample_record("/tmp/x/SKILL.md"));
        state.save(&path).unwrap();

        let loaded = StateFile::load(&path).unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn upsert_replaces_existing_record_for_same_path() {
        let mut state = StateFile::default();
        state.upsert(sample_record("/tmp/x/SKILL.md"));
        let mut updated = sample_record("/tmp/x/SKILL.md");
        updated.skill_version = "2.0.0".to_string();
        state.upsert(updated);

        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(state.artifacts[0].skill_version, "2.0.0");
    }

    #[test]
    fn distinct_sub_keys_at_the_same_path_do_not_collide() {
        // Regression test: the MCP adapter emits one `MergeJson` artifact
        // per dependency, and multiple dependencies of one skill share a
        // path (e.g. `.vscode/mcp.json`). Keying only on `path` would let
        // the second dependency's record silently clobber the first's.
        let mut state = StateFile::default();
        let mut a = sample_record("/workspace/.vscode/mcp.json");
        a.kind = ArtifactKind::MergeJson;
        a.sub_key = Some("/mcpServers/rust-analyzer-mcp".to_string());
        let mut b = a.clone();
        b.sub_key = Some("/mcpServers/other".to_string());

        state.upsert(a.clone());
        state.upsert(b.clone());

        assert_eq!(state.artifacts.len(), 2);
        assert_eq!(state.record_for(&a.path, a.sub_key.as_deref()), Some(&a));
        assert_eq!(state.record_for(&b.path, b.sub_key.as_deref()), Some(&b));
    }

    #[test]
    fn remove_skill_drops_only_that_skills_records() {
        let mut state = StateFile::default();
        state.upsert(sample_record("/tmp/a/SKILL.md"));
        let mut other = sample_record("/tmp/b/SKILL.md");
        other.skill = "other-skill".to_string();
        state.upsert(other);

        let removed = state.remove_skill("rust-systems-expert");
        assert_eq!(removed.len(), 1);
        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(state.artifacts[0].skill, "other-skill");
    }
}
