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
        // Boxed: `toml::de::Error` is 96 bytes on its own, which alone put
        // `SkillError` over clippy's `result_large_err` threshold on
        // Windows and made every `Result` in the crate carry it.
        #[source]
        source: Box<toml::de::Error>,
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
        source: Box<toml::de::Error>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `SkillError` rides in the `Err` arm of nearly every function in this
    /// crate, so its size is a cost paid on every call.
    ///
    /// This exists because it silently grew past clippy's 128-byte
    /// `result_large_err` threshold — and only failed CI on Windows, whose
    /// `io::Error` is wider than Unix's. A local `cargo clippy` was clean
    /// while the build was already broken on a platform most contributors
    /// won't have. Asserting the size directly catches it everywhere.
    #[test]
    fn stays_small_enough_to_return_by_value() {
        let size = std::mem::size_of::<SkillError>();
        assert!(
            size <= 128,
            "SkillError grew to {size} bytes; clippy::result_large_err fires above 128. \
             Box the offending variant's payload rather than raising this bound."
        );
    }
}
