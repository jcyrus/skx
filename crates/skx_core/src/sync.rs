//! The write engine: turns an [`Artifact`] into an actual filesystem
//! change, and reports what was written so [`crate::state`] can fingerprint
//! it. This is the only place `skx` touches disk on behalf of an adapter —
//! adapters only ever describe what they want; this module is what
//! actually mutates the workspace.

use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

use crate::artifact::{Artifact, LinkStrategy};
use crate::error::{Result, SkillError};
use crate::state::{ArtifactKind, ArtifactRecord, hash_content};

/// What [`apply`] actually did, used to build the [`crate::ArtifactRecord`]
/// that gets persisted to state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    /// sha256 of the content that now represents this artifact: the
    /// resolved symlink target's bytes for a symlink, the whole file for a
    /// plain `OwnedFile` write, or just the region/value text for
    /// `Region`/`MergeJson`.
    pub content_hash: String,
    /// Set when this write was a symlink rather than a plain file.
    pub symlink_target: Option<PathBuf>,
}

/// Materializes `artifact` on disk.
///
/// `cache_source` is the skill's canonical file in the cache; it's required
/// (and used instead of `artifact`'s own `contents`) whenever `strategy` is
/// [`LinkStrategy::Symlink`], since a symlink mirrors the source verbatim —
/// using the adapter-rendered `contents` there would let a re-render that
/// isn't byte-identical to the cache file silently diverge from what's
/// actually on disk.
pub fn apply(
    artifact: &Artifact,
    strategy: LinkStrategy,
    cache_source: Option<&Path>,
) -> Result<WriteResult> {
    match artifact {
        Artifact::OwnedFile { path, contents } => {
            create_parent_dirs(path)?;
            match strategy {
                LinkStrategy::Symlink => {
                    let target = cache_source
                        .expect("LinkStrategy::Symlink requires a cache_source to link against");
                    let linked = replace_with_symlink(path, target)?;
                    let resolved = std::fs::read(target).map_err(|source| SkillError::Io {
                        path: target.to_path_buf(),
                        source,
                    })?;
                    if !linked {
                        // Fell back to a copy (see `replace_with_symlink`).
                        // Recording `symlink_target: None` is what keeps
                        // drift detection honest: the artifact is now a
                        // plain file, so it must be audited by content hash
                        // rather than by where a link points.
                        std::fs::write(path, &resolved).map_err(|source| SkillError::Io {
                            path: path.to_path_buf(),
                            source,
                        })?;
                    }
                    Ok(WriteResult {
                        content_hash: hash_content(&resolved),
                        symlink_target: linked.then(|| target.to_path_buf()),
                    })
                }
                LinkStrategy::Compile => {
                    replace_with_file(path)?;
                    std::fs::write(path, contents).map_err(|source| SkillError::Io {
                        path: path.to_path_buf(),
                        source,
                    })?;
                    Ok(WriteResult {
                        content_hash: hash_content(contents.as_bytes()),
                        symlink_target: None,
                    })
                }
            }
        }
        Artifact::Region {
            path,
            marker,
            contents,
        } => {
            create_parent_dirs(path)?;
            let merged = merge_region(path, marker, contents)?;
            std::fs::write(path, &merged).map_err(|source| SkillError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            // Hash the same normalized text `merge_region` actually embeds
            // (trailing whitespace trimmed) — not the raw `contents` — so
            // this matches what `read_current_bytes` extracts back out on
            // audit. Hashing the untrimmed string here would make every
            // Region artifact look user-modified immediately after sync.
            Ok(WriteResult {
                content_hash: hash_content(contents.trim_end().as_bytes()),
                symlink_target: None,
            })
        }
        Artifact::MergeJson {
            path,
            pointer,
            value,
        } => {
            create_parent_dirs(path)?;
            let merged = merge_json(path, pointer, value)?;
            let bytes = serde_json::to_vec_pretty(&merged)
                .map_err(|source| SkillError::SerializeJson { source })?;
            std::fs::write(path, &bytes).map_err(|source| SkillError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            let value_bytes =
                serde_json::to_vec(value).map_err(|source| SkillError::SerializeJson { source })?;
            Ok(WriteResult {
                content_hash: hash_content(&value_bytes),
                symlink_target: None,
            })
        }
    }
}

/// Where one recorded artifact stands relative to disk right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftStatus {
    /// Disk matches what skx last wrote, and (if checked) matches what
    /// compiling the skill right now would produce.
    InSync,
    /// Disk matches what skx last wrote, but recompiling the skill now
    /// would produce something different — the skill changed upstream and
    /// `sync` hasn't run since. Safe to re-sync.
    UpstreamChanged,
    /// Disk doesn't match what skx last wrote. Someone (or something)
    /// edited the artifact directly; re-syncing would clobber that edit.
    UserModified,
    /// The artifact (file, or symlink) no longer exists on disk.
    Missing,
}

/// Compares `record` against what's actually on disk, and — if the caller
/// passes the hash of what compiling the skill *right now* would produce —
/// against that too, to tell "needs a sync" apart from "someone edited
/// this by hand". Pass `None` for `fresh_hash` to skip that distinction
/// (e.g. when the skill itself is gone and there's nothing to recompile).
pub fn audit_record(record: &ArtifactRecord, fresh_hash: Option<&str>) -> Result<DriftStatus> {
    if let Some(expected_target) = &record.symlink_target {
        return Ok(match std::fs::read_link(&record.path) {
            Ok(actual) if &actual == expected_target => DriftStatus::InSync,
            Ok(_) => DriftStatus::UserModified,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DriftStatus::Missing,
            // Path exists but isn't a symlink anymore — someone replaced
            // the link with a real file.
            Err(_) => DriftStatus::UserModified,
        });
    }

    let Some(disk_bytes) = read_current_bytes(record)? else {
        return Ok(DriftStatus::Missing);
    };
    let disk_hash = hash_content(&disk_bytes);

    if disk_hash != record.content_hash {
        return Ok(DriftStatus::UserModified);
    }
    Ok(match fresh_hash {
        Some(fresh) if fresh != record.content_hash => DriftStatus::UpstreamChanged,
        _ => DriftStatus::InSync,
    })
}

/// Reads back just the slice of disk content `record` is responsible for:
/// the whole file for `OwnedFile`, the marked region's inner text for
/// `Region` (keyed by `record.sub_key` as the marker), or the JSON value at
/// the pointer for `MergeJson` (keyed by `record.sub_key` as the pointer).
/// Returns `None` if the file, region, or pointer is missing.
fn read_current_bytes(record: &ArtifactRecord) -> Result<Option<Vec<u8>>> {
    match record.kind {
        ArtifactKind::OwnedFile => match std::fs::read(&record.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SkillError::Io {
                path: record.path.clone(),
                source,
            }),
        },
        ArtifactKind::Region => {
            let marker = record.sub_key.as_deref().unwrap_or_default();
            let existing = match std::fs::read_to_string(&record.path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(source) => {
                    return Err(SkillError::Io {
                        path: record.path.clone(),
                        source,
                    });
                }
            };
            let start_marker = region_start_marker(marker);
            let end_marker = region_end_marker(marker);
            match (existing.find(&start_marker), existing.find(&end_marker)) {
                (Some(start_idx), Some(end_idx)) if end_idx > start_idx => {
                    let inner_start = start_idx + start_marker.len();
                    let inner = existing[inner_start..end_idx].trim_matches('\n');
                    Ok(Some(inner.as_bytes().to_vec()))
                }
                _ => Ok(None),
            }
        }
        ArtifactKind::MergeJson => {
            let pointer = record.sub_key.as_deref().unwrap_or("");
            let existing: JsonValue = match std::fs::read_to_string(&record.path) {
                Ok(s) if s.trim().is_empty() => return Ok(None),
                Ok(s) => serde_json::from_str(&s).map_err(|source| SkillError::InvalidJson {
                    path: record.path.clone(),
                    source,
                })?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(source) => {
                    return Err(SkillError::Io {
                        path: record.path.clone(),
                        source,
                    });
                }
            };
            match existing.pointer(pointer) {
                Some(value) => {
                    let bytes = serde_json::to_vec(value)
                        .map_err(|source| SkillError::SerializeJson { source })?;
                    Ok(Some(bytes))
                }
                None => Ok(None),
            }
        }
    }
}

/// The hash [`apply`] would produce for `artifact` right now, without
/// writing anything. Used by `skx audit` to tell "needs a sync" apart from
/// "someone edited this by hand" — see [`audit_record`] — without the side
/// effect of actually performing the sync.
pub fn fresh_hash(
    artifact: &Artifact,
    strategy: LinkStrategy,
    cache_source: Option<&Path>,
) -> Result<String> {
    match artifact {
        Artifact::OwnedFile { contents, .. } => match strategy {
            LinkStrategy::Symlink => {
                let target = cache_source
                    .expect("LinkStrategy::Symlink requires a cache_source to hash against");
                let bytes = std::fs::read(target).map_err(|source| SkillError::Io {
                    path: target.to_path_buf(),
                    source,
                })?;
                Ok(hash_content(&bytes))
            }
            LinkStrategy::Compile => Ok(hash_content(contents.as_bytes())),
        },
        Artifact::Region { contents, .. } => Ok(hash_content(contents.trim_end().as_bytes())),
        Artifact::MergeJson { value, .. } => {
            let bytes =
                serde_json::to_vec(value).map_err(|source| SkillError::SerializeJson { source })?;
            Ok(hash_content(&bytes))
        }
    }
}

/// Removes an `OwnedFile` artifact (plain file or symlink) at `path`, if
/// present. Used by `skx remove` to undo a previous sync.
pub fn remove_owned_file(path: &Path) -> Result<()> {
    remove_existing(path)
}

/// Removes the `<!-- skx:start {marker} -->`..`<!-- skx:end {marker} -->`
/// block from the file at `path`, if present, preserving everything else
/// in the file. No-op if the file or marker doesn't exist.
pub fn remove_region(path: &Path, marker: &str) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SkillError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let start_marker = region_start_marker(marker);
    let end_marker = region_end_marker(marker);
    let (Some(start_idx), Some(end_idx)) =
        (existing.find(&start_marker), existing.find(&end_marker))
    else {
        return Ok(());
    };

    let after_end = end_idx + end_marker.len();
    let mut result = String::with_capacity(existing.len());
    result.push_str(&existing[..start_idx]);
    result.push_str(existing[after_end..].trim_start_matches('\n'));
    std::fs::write(path, result).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Removes the value at `pointer` (RFC 6901) from the JSON document at
/// `path`, if present, preserving every other key. No-op if the file or
/// pointer doesn't exist.
pub fn remove_json_pointer(path: &Path, pointer: &str) -> Result<()> {
    let mut root: JsonValue = match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => return Ok(()),
        Ok(s) => serde_json::from_str(&s).map_err(|source| SkillError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SkillError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let segments: Vec<String> = pointer
        .split('/')
        .filter(|s| !s.is_empty())
        .map(unescape_pointer_token)
        .collect();
    if let Some((last, ancestors)) = segments.split_last() {
        let mut current = &mut root;
        for seg in ancestors {
            match current.get_mut(seg) {
                Some(next) => current = next,
                None => return Ok(()),
            }
        }
        if let Some(obj) = current.as_object_mut() {
            obj.remove(last);
        }
    }

    let bytes =
        serde_json::to_vec_pretty(&root).map_err(|source| SkillError::SerializeJson { source })?;
    std::fs::write(path, bytes).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_parent_dirs(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SkillError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// Removes whatever currently sits at `path` (file or symlink) and puts a
/// symlink to `target` in its place.
///
/// Returns `false` when the platform refused to create the link and the
/// caller should write a copy instead.
///
/// Windows only creates symlinks for accounts with Developer Mode enabled
/// or an elevated prompt, so on an ordinary desktop every sync would fail
/// with a bare "A required privilege is not held by the client". Degrading
/// to a copy keeps `skx` usable there. The cost is real and not hidden: a
/// copy doesn't track later edits to the cached skill, so the artifact
/// shows up as stale on the next `skx audit` and needs a re-sync — which is
/// exactly what a copy *should* do, and why the fallback is reported rather
/// than silently swallowed.
fn replace_with_symlink(path: &Path, target: &Path) -> Result<bool> {
    remove_existing(path)?;
    match symlink(target, path) {
        Ok(()) => Ok(true),
        Err(source) if is_unsupported_symlink(&source) => Ok(false),
        Err(source) => Err(SkillError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Whether an error means "this platform won't let me symlink" rather than
/// something genuinely wrong (a missing parent, a full disk).
///
/// Only ever true on Windows: on Unix a `PermissionDenied` here means the
/// destination really is unwritable, and quietly writing a copy would hide
/// a problem the user needs to know about.
fn is_unsupported_symlink(error: &std::io::Error) -> bool {
    cfg!(windows)
        && matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
        )
}

/// Removes whatever currently sits at `path` if it's a symlink, so a plain
/// write doesn't write through a stale link left by a previous sync.
fn replace_with_file(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        remove_existing(path)?;
    }
    Ok(())
}

fn remove_existing(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::remove_file(path).map_err(|source| SkillError::Io {
            path: path.to_path_buf(),
            source,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SkillError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

const REGION_START_PREFIX: &str = "<!-- skx:start ";
const REGION_END_PREFIX: &str = "<!-- skx:end ";

fn region_start_marker(marker: &str) -> String {
    format!("{REGION_START_PREFIX}{marker} -->")
}

fn region_end_marker(marker: &str) -> String {
    format!("{REGION_END_PREFIX}{marker} -->")
}

/// Replaces the region between `<!-- skx:start {marker} -->` and
/// `<!-- skx:end {marker} -->` inside the file at `path` with `contents`,
/// preserving everything else in the file. If the file doesn't exist, or
/// doesn't yet contain that marker pair, the block is appended.
fn merge_region(path: &Path, marker: &str, contents: &str) -> Result<String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(SkillError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let start_marker = region_start_marker(marker);
    let end_marker = region_end_marker(marker);
    let block = format!("{start_marker}\n{}\n{end_marker}\n", contents.trim_end());

    if let (Some(start_idx), Some(end_idx)) =
        (existing.find(&start_marker), existing.find(&end_marker))
    {
        let after_end = end_idx + end_marker.len();
        let mut merged = String::with_capacity(existing.len() + block.len());
        merged.push_str(&existing[..start_idx]);
        merged.push_str(&block);
        let rest = existing[after_end..].trim_start_matches('\n');
        merged.push_str(rest);
        Ok(merged)
    } else {
        let mut merged = existing;
        if !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        if !merged.is_empty() {
            merged.push('\n');
        }
        merged.push_str(&block);
        Ok(merged)
    }
}

/// Sets `value` at `pointer` (RFC 6901) inside the JSON document at `path`,
/// creating missing intermediate objects and preserving every other key.
/// If the file doesn't exist or is empty, starts from `{}`.
fn merge_json(path: &Path, pointer: &str, value: &JsonValue) -> Result<JsonValue> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => JsonValue::Object(Default::default()),
        Ok(s) => serde_json::from_str(&s).map_err(|source| SkillError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => JsonValue::Object(Default::default()),
        Err(source) => {
            return Err(SkillError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let mut root = existing;
    set_at_pointer(&mut root, pointer, value.clone());
    Ok(root)
}

fn unescape_pointer_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn set_at_pointer(root: &mut JsonValue, pointer: &str, value: JsonValue) {
    let segments: Vec<String> = pointer
        .split('/')
        .filter(|s| !s.is_empty())
        .map(unescape_pointer_token)
        .collect();

    let Some((last, ancestors)) = segments.split_last() else {
        *root = value;
        return;
    };

    let mut current = root;
    for seg in ancestors {
        if !current.is_object() {
            *current = JsonValue::Object(Default::default());
        }
        current = current
            .as_object_mut()
            .expect("just ensured this is an object")
            .entry(seg.clone())
            .or_insert_with(|| JsonValue::Object(Default::default()));
    }
    if !current.is_object() {
        *current = JsonValue::Object(Default::default());
    }
    current
        .as_object_mut()
        .expect("just ensured this is an object")
        .insert(last.clone(), value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owned_file_compile_writes_contents_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/SKILL.md");
        let artifact = Artifact::OwnedFile {
            path: path.clone(),
            contents: "hello".to_string(),
        };
        let result = apply(&artifact, LinkStrategy::Compile, None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        assert_eq!(result.content_hash, hash_content(b"hello"));
        assert!(result.symlink_target.is_none());
    }

    #[test]
    fn owned_file_symlink_links_to_cache_and_hashes_resolved_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/SKILL.md");
        create_parent_dirs(&cache).unwrap();
        std::fs::write(&cache, "canonical content").unwrap();

        let link_path = dir.path().join("out/SKILL.md");
        let artifact = Artifact::OwnedFile {
            path: link_path.clone(),
            // Deliberately different from the cache file's bytes, to prove
            // this content is ignored for the Symlink strategy.
            contents: "stale re-render".to_string(),
        };
        let result = apply(&artifact, LinkStrategy::Symlink, Some(&cache)).unwrap();

        assert!(
            link_path
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&link_path).unwrap(), cache);
        assert_eq!(result.content_hash, hash_content(b"canonical content"));
        assert_eq!(result.symlink_target, Some(cache));
    }

    #[test]
    fn a_unix_symlink_failure_is_reported_rather_than_copied_over() {
        // The Windows fallback must not mask a genuine problem on Unix: a
        // permission error there means the destination really is
        // unwritable, and writing a copy anyway would hide it.
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(is_unsupported_symlink(&denied), cfg!(windows));

        let disk_full = std::io::Error::other("no space left on device");
        assert!(!is_unsupported_symlink(&disk_full));
    }

    #[test]
    fn symlink_replaces_a_previously_written_plain_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/SKILL.md");
        create_parent_dirs(&cache).unwrap();
        std::fs::write(&cache, "v2").unwrap();

        let path = dir.path().join("out/SKILL.md");
        create_parent_dirs(&path).unwrap();
        std::fs::write(&path, "stale plain file").unwrap();

        let artifact = Artifact::OwnedFile {
            path: path.clone(),
            contents: "v2".to_string(),
        };
        apply(&artifact, LinkStrategy::Symlink, Some(&cache)).unwrap();
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn region_appends_when_file_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        let artifact = Artifact::Region {
            path: path.clone(),
            marker: "my-skill".to_string(),
            contents: "## instructions\nbe helpful".to_string(),
        };
        apply(&artifact, LinkStrategy::Compile, None).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("<!-- skx:start my-skill -->"));
        assert!(written.contains("be helpful"));
        assert!(written.contains("<!-- skx:end my-skill -->"));
    }

    #[test]
    fn region_replaces_existing_block_and_preserves_surrounding_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        std::fs::write(
            &path,
            "# User instructions\nsome hand-written text\n\n<!-- skx:start my-skill -->\nold body\n<!-- skx:end my-skill -->\n\nmore hand-written text\n",
        )
        .unwrap();

        let artifact = Artifact::Region {
            path: path.clone(),
            marker: "my-skill".to_string(),
            contents: "new body".to_string(),
        };
        apply(&artifact, LinkStrategy::Compile, None).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("some hand-written text"));
        assert!(written.contains("more hand-written text"));
        assert!(written.contains("new body"));
        assert!(!written.contains("old body"));
    }

    #[test]
    fn region_only_touches_its_own_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        std::fs::write(
            &path,
            "<!-- skx:start other-skill -->\nother body\n<!-- skx:end other-skill -->\n",
        )
        .unwrap();

        let artifact = Artifact::Region {
            path: path.clone(),
            marker: "my-skill".to_string(),
            contents: "my body".to_string(),
        };
        apply(&artifact, LinkStrategy::Compile, None).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("other body"));
        assert!(written.contains("my body"));
    }

    #[test]
    fn merge_json_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let artifact = Artifact::MergeJson {
            path: path.clone(),
            pointer: "/mcpServers/rust-analyzer-mcp".to_string(),
            value: json!({"command": "rust-analyzer-mcp"}),
        };
        apply(&artifact, LinkStrategy::Compile, None).unwrap();

        let written: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["rust-analyzer-mcp"]["command"],
            "rust-analyzer-mcp"
        );
    }

    #[test]
    fn merge_json_preserves_other_keys_and_other_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "unrelatedTopLevelKey": true,
                "mcpServers": { "existing-server": { "command": "existing" } }
            }))
            .unwrap(),
        )
        .unwrap();

        let artifact = Artifact::MergeJson {
            path: path.clone(),
            pointer: "/mcpServers/new-server".to_string(),
            value: json!({"command": "new"}),
        };
        apply(&artifact, LinkStrategy::Compile, None).unwrap();

        let written: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["unrelatedTopLevelKey"], true);
        assert_eq!(
            written["mcpServers"]["existing-server"]["command"],
            "existing"
        );
        assert_eq!(written["mcpServers"]["new-server"]["command"], "new");
    }

    #[test]
    fn two_dependencies_at_the_same_path_both_survive() {
        // Mirrors what McpAdapter does for a skill with 2+ mcp_dependencies:
        // two `apply` calls at the same path, different pointers.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");

        apply(
            &Artifact::MergeJson {
                path: path.clone(),
                pointer: "/mcpServers/a".to_string(),
                value: json!({"command": "cmd-a"}),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();
        apply(
            &Artifact::MergeJson {
                path: path.clone(),
                pointer: "/mcpServers/b".to_string(),
                value: json!({"command": "cmd-b"}),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();

        let written: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["a"]["command"], "cmd-a");
        assert_eq!(written["mcpServers"]["b"]["command"], "cmd-b");
    }

    fn base_record(kind: ArtifactKind, path: PathBuf) -> ArtifactRecord {
        ArtifactRecord {
            path,
            sub_key: None,
            skill: "s".to_string(),
            skill_version: "1.0.0".to_string(),
            target: "t".to_string(),
            kind,
            content_hash: String::new(),
            symlink_target: None,
        }
    }

    #[test]
    fn audit_owned_file_in_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/SKILL.md");
        let artifact = Artifact::OwnedFile {
            path: path.clone(),
            contents: "v1".to_string(),
        };
        let write = apply(&artifact, LinkStrategy::Compile, None).unwrap();
        let mut record = base_record(ArtifactKind::OwnedFile, path);
        record.content_hash = write.content_hash.clone();

        assert_eq!(
            audit_record(&record, Some(&write.content_hash)).unwrap(),
            DriftStatus::InSync
        );
    }

    #[test]
    fn audit_owned_file_detects_user_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "user changed this").unwrap();

        let mut record = base_record(ArtifactKind::OwnedFile, path);
        record.content_hash = hash_content(b"v1");

        assert_eq!(
            audit_record(&record, Some(&hash_content(b"v1"))).unwrap(),
            DriftStatus::UserModified
        );
    }

    #[test]
    fn audit_owned_file_detects_upstream_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "v1").unwrap();

        let mut record = base_record(ArtifactKind::OwnedFile, path);
        record.content_hash = hash_content(b"v1");

        // Disk still matches what we last wrote, but a fresh compile
        // would now produce "v2" — the skill changed upstream.
        assert_eq!(
            audit_record(&record, Some(&hash_content(b"v2"))).unwrap(),
            DriftStatus::UpstreamChanged
        );
    }

    #[test]
    fn audit_owned_file_detects_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out/SKILL.md");
        let mut record = base_record(ArtifactKind::OwnedFile, path);
        record.content_hash = hash_content(b"v1");

        assert_eq!(
            audit_record(&record, Some(&hash_content(b"v1"))).unwrap(),
            DriftStatus::Missing
        );
    }

    #[test]
    fn audit_symlink_in_sync_checks_link_target_not_content() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/SKILL.md");
        create_parent_dirs(&cache).unwrap();
        std::fs::write(&cache, "canonical").unwrap();
        let path = dir.path().join("out/SKILL.md");
        let write = apply(
            &Artifact::OwnedFile {
                path: path.clone(),
                contents: "ignored".to_string(),
            },
            LinkStrategy::Symlink,
            Some(&cache),
        )
        .unwrap();

        let mut record = base_record(ArtifactKind::OwnedFile, path);
        record.content_hash = write.content_hash;
        record.symlink_target = write.symlink_target;

        assert_eq!(audit_record(&record, None).unwrap(), DriftStatus::InSync);
    }

    #[test]
    fn audit_symlink_detects_retargeting() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/SKILL.md");
        create_parent_dirs(&cache).unwrap();
        std::fs::write(&cache, "canonical").unwrap();
        let other = dir.path().join("other/SKILL.md");
        create_parent_dirs(&other).unwrap();
        std::fs::write(&other, "different skill").unwrap();

        let path = dir.path().join("out/SKILL.md");
        apply(
            &Artifact::OwnedFile {
                path: path.clone(),
                contents: "ignored".to_string(),
            },
            LinkStrategy::Symlink,
            Some(&other),
        )
        .unwrap();

        let mut record = base_record(ArtifactKind::OwnedFile, path);
        record.symlink_target = Some(cache);

        assert_eq!(
            audit_record(&record, None).unwrap(),
            DriftStatus::UserModified
        );
    }

    #[test]
    fn audit_region_extracts_only_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        let write = apply(
            &Artifact::Region {
                path: path.clone(),
                marker: "my-skill".to_string(),
                contents: "body v1".to_string(),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();

        let mut record = base_record(ArtifactKind::Region, path.clone());
        record.sub_key = Some("my-skill".to_string());
        record.content_hash = write.content_hash;

        assert_eq!(
            audit_record(&record, Some(&hash_content(b"body v1"))).unwrap(),
            DriftStatus::InSync
        );

        // Someone hand-edits the region in the shared file.
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, raw.replace("body v1", "hand edited")).unwrap();
        assert_eq!(
            audit_record(&record, Some(&hash_content(b"body v1"))).unwrap(),
            DriftStatus::UserModified
        );
    }

    #[test]
    fn region_with_trailing_newline_is_in_sync_immediately_after_apply() {
        // Regression test: `apply` used to hash the raw, untrimmed
        // `contents` for a Region artifact, but `merge_region` embeds
        // `contents.trim_end()` and `read_current_bytes` extracts that same
        // trimmed text back out on audit. Real adapters (e.g. Copilot)
        // always produce `contents` ending in '\n', so this mismatch made
        // every Region artifact register as user-modified on the very
        // first audit after a sync that never touched the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        let contents = "## my-skill\n\nsome instructions\n".to_string();
        let write = apply(
            &Artifact::Region {
                path: path.clone(),
                marker: "my-skill".to_string(),
                contents: contents.clone(),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();

        let mut record = base_record(ArtifactKind::Region, path);
        record.sub_key = Some("my-skill".to_string());
        record.content_hash = write.content_hash;

        let fresh = fresh_hash(
            &Artifact::Region {
                path: PathBuf::new(),
                marker: "my-skill".to_string(),
                contents,
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();

        assert_eq!(
            audit_record(&record, Some(&fresh)).unwrap(),
            DriftStatus::InSync
        );
    }

    #[test]
    fn audit_merge_json_reads_back_the_right_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        apply(
            &Artifact::MergeJson {
                path: path.clone(),
                pointer: "/mcpServers/a".to_string(),
                value: json!({"command": "cmd-a"}),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();
        apply(
            &Artifact::MergeJson {
                path: path.clone(),
                pointer: "/mcpServers/b".to_string(),
                value: json!({"command": "cmd-b"}),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();

        let mut record_a = base_record(ArtifactKind::MergeJson, path.clone());
        record_a.sub_key = Some("/mcpServers/a".to_string());
        record_a.content_hash =
            hash_content(&serde_json::to_vec(&json!({"command": "cmd-a"})).unwrap());
        assert_eq!(
            audit_record(&record_a, Some(&record_a.content_hash)).unwrap(),
            DriftStatus::InSync
        );

        // Editing server "b" shouldn't affect server "a"'s audit status.
        let mut root: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        root["mcpServers"]["b"]["command"] = json!("tampered");
        std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();

        assert_eq!(
            audit_record(&record_a, Some(&record_a.content_hash)).unwrap(),
            DriftStatus::InSync
        );
    }

    #[test]
    fn fresh_hash_owned_file_compile_matches_apply() {
        let artifact = Artifact::OwnedFile {
            path: PathBuf::from("/out/SKILL.md"),
            contents: "v1".to_string(),
        };
        assert_eq!(
            fresh_hash(&artifact, LinkStrategy::Compile, None).unwrap(),
            hash_content(b"v1")
        );
    }

    #[test]
    fn fresh_hash_symlink_reads_cache_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/SKILL.md");
        create_parent_dirs(&cache).unwrap();
        std::fs::write(&cache, "canonical").unwrap();

        let artifact = Artifact::OwnedFile {
            path: dir.path().join("out/SKILL.md"),
            contents: "ignored".to_string(),
        };
        let hash = fresh_hash(&artifact, LinkStrategy::Symlink, Some(&cache)).unwrap();
        assert_eq!(hash, hash_content(b"canonical"));
        assert!(!dir.path().join("out/SKILL.md").exists());
    }

    #[test]
    fn remove_owned_file_deletes_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache/SKILL.md");
        create_parent_dirs(&cache).unwrap();
        std::fs::write(&cache, "x").unwrap();
        let path = dir.path().join("out/SKILL.md");
        apply(
            &Artifact::OwnedFile {
                path: path.clone(),
                contents: "x".to_string(),
            },
            LinkStrategy::Symlink,
            Some(&cache),
        )
        .unwrap();

        remove_owned_file(&path).unwrap();
        assert!(path.symlink_metadata().is_err());
        assert!(
            cache.exists(),
            "removing the link must not touch the cache source"
        );
    }

    #[test]
    fn remove_region_strips_block_and_preserves_surrounding_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        std::fs::write(
            &path,
            "before\n\n<!-- skx:start my-skill -->\nbody\n<!-- skx:end my-skill -->\n\nafter\n",
        )
        .unwrap();

        remove_region(&path, "my-skill").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("before"));
        assert!(written.contains("after"));
        assert!(!written.contains("body"));
        assert!(!written.contains("skx:start"));
    }

    #[test]
    fn remove_region_is_a_noop_when_marker_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copilot-instructions.md");
        std::fs::write(&path, "untouched\n").unwrap();
        remove_region(&path, "nonexistent").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "untouched\n");
    }

    #[test]
    fn remove_json_pointer_deletes_only_that_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        apply(
            &Artifact::MergeJson {
                path: path.clone(),
                pointer: "/mcpServers/a".to_string(),
                value: json!({"command": "cmd-a"}),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();
        apply(
            &Artifact::MergeJson {
                path: path.clone(),
                pointer: "/mcpServers/b".to_string(),
                value: json!({"command": "cmd-b"}),
            },
            LinkStrategy::Compile,
            None,
        )
        .unwrap();

        remove_json_pointer(&path, "/mcpServers/a").unwrap();

        let written: JsonValue =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written["mcpServers"].get("a").is_none());
        assert_eq!(written["mcpServers"]["b"]["command"], "cmd-b");
    }
}
