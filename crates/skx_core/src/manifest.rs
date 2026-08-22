//! The project-level `skx.toml` manifest: which skills `skx add` has
//! registered, at what scope, and where they came from.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SkillError};
use crate::model::Scope;

/// Where the project-level manifest lives, given a workspace root. Shared
/// by `skx_cli` and `skx_tui` so both resolve the same file.
pub fn manifest_path(root: &Path) -> std::path::PathBuf {
    root.join("skx.toml")
}

/// One skill `skx add` has registered. Tracked here so `sync`/`remove`
/// know where it came from and at what scope without re-deriving that
/// from the cache layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub name: String,
    /// What the user passed to `skx add` — a local path today; a git URL
    /// once that's supported. Informational only: the cached copy's
    /// location is always derived from `(scope, name)` via
    /// `skx_core::cache`, never from this field.
    pub source: String,
    pub scope: Scope,
    pub version: String,
}

/// Persisted at `<workspace root>/skx.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Agent target ids `skx init` found evidence of in this workspace
    /// (e.g. an existing `.cursor/` directory). Informational — doesn't
    /// gate which adapters `sync` runs, since that's driven entirely by
    /// what each installed skill's own `targets` block declares.
    #[serde(default)]
    pub detected_targets: Vec<String>,
    #[serde(default)]
    pub skills: Vec<ManifestEntry>,
}

impl Manifest {
    /// Loads the manifest from `path`, or returns an empty one if it
    /// doesn't exist yet (before the first `skx add`).
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|source| SkillError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| SkillError::InvalidManifest {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).map_err(|source| SkillError::SerializeManifest {
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

    pub fn get(&self, name: &str) -> Option<&ManifestEntry> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Inserts `entry`, replacing any existing entry with the same name.
    pub fn upsert(&mut self, entry: ManifestEntry) {
        match self.skills.iter_mut().find(|s| s.name == entry.name) {
            Some(existing) => *existing = entry,
            None => self.skills.push(entry),
        }
    }

    /// Removes the entry named `name`, if present, returning it.
    pub fn remove(&mut self, name: &str) -> Option<ManifestEntry> {
        let idx = self.skills.iter().position(|s| s.name == name)?;
        Some(self.skills.remove(idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(name: &str) -> ManifestEntry {
        ManifestEntry {
            name: name.to_string(),
            source: "/some/local/path".to_string(),
            scope: Scope::Local,
            version: "1.0.0".to_string(),
        }
    }

    #[test]
    fn missing_manifest_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest::load(&dir.path().join("skx.toml")).unwrap();
        assert!(manifest.skills.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skx.toml");

        let mut manifest = Manifest::default();
        manifest.upsert(sample_entry("foo"));
        manifest.save(&path).unwrap();

        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded, manifest);
    }

    #[test]
    fn upsert_replaces_existing_entry_by_name() {
        let mut manifest = Manifest::default();
        manifest.upsert(sample_entry("foo"));
        let mut updated = sample_entry("foo");
        updated.version = "2.0.0".to_string();
        manifest.upsert(updated);

        assert_eq!(manifest.skills.len(), 1);
        assert_eq!(manifest.skills[0].version, "2.0.0");
    }

    #[test]
    fn remove_deletes_and_returns_entry() {
        let mut manifest = Manifest::default();
        manifest.upsert(sample_entry("foo"));
        let removed = manifest.remove("foo").expect("entry should exist");
        assert_eq!(removed.name, "foo");
        assert!(manifest.get("foo").is_none());
    }

    #[test]
    fn remove_missing_entry_returns_none() {
        let mut manifest = Manifest::default();
        assert!(manifest.remove("nope").is_none());
    }
}
