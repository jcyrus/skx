//! Core domain model, parser, and discovery logic for `skx`.
//!
//! This crate defines the canonical `SKILL.md` schema and provides the
//! parsing/serialization primitives that `skx_adapters`, `skx_tui`, and
//! `skx_cli` build on. It intentionally knows nothing about specific
//! target agents (Antigravity, Claude Code, Cursor, ...) — that lives in
//! `skx_adapters`. Per-target config in [`Frontmatter::targets`] is kept
//! deliberately opaque for the same reason.

pub mod artifact;
pub mod cache;
pub mod config;
pub mod discover;
pub mod error;
pub mod manifest;
pub mod model;
pub mod parser;
pub mod state;
pub mod sync;

pub use artifact::{Artifact, CompileCtx, CompiledOutput, LinkStrategy};
pub use cache::{cache_dir, skill_path};
pub use config::{Config, ThemePreference, config_path};
pub use discover::{
    DiscoveredSkill, FoundIn, default_pick, group_by_name, scan_for_unmanaged_skills,
};
pub use error::{Result, SkillError};
pub use manifest::{Manifest, ManifestEntry, manifest_path};
pub use model::{Frontmatter, McpDependency, Scope, Skill, SkillName, unknown_targets};
pub use parser::{discover_skills, load_skill, parse_skill, render_skill, split_frontmatter};
pub use state::{
    ArtifactKind, ArtifactRecord, StateFile, artifact_kind_and_sub_key, hash_content, state_path,
};
pub use sync::{
    DriftStatus, WriteResult, apply, audit_record, fresh_hash, remove_json_pointer,
    remove_owned_file, remove_region,
};
