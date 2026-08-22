use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("skill file {0} has no frontmatter (expected content to start with '---')")]
    MissingFrontmatter(PathBuf),

    #[error("skill file {0} has an unterminated frontmatter block (missing closing '---')")]
    UnterminatedFrontmatter(PathBuf),

    #[error("failed to parse frontmatter YAML in {path}: {source}")]
    InvalidFrontmatter {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("failed to read skill file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to walk directory {path}: {source}")]
    Walk {
        path: PathBuf,
        #[source]
        source: walkdir::Error,
    },

    #[error("failed to serialize skill {name}: {source}")]
    Serialize {
        name: String,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("invalid skill name {name:?}: {reason}")]
    InvalidName { name: String, reason: String },

    #[error("failed to parse state file {path}: {source}")]
    InvalidState {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize state file {path}: {source}")]
    SerializeState {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },

    #[error("failed to parse manifest {path}: {source}")]
    InvalidManifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize manifest {path}: {source}")]
    SerializeManifest {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },

    #[error("failed to parse JSON config {path}: {source}")]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to serialize JSON config: {source}")]
    SerializeJson {
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, SkillError>;
