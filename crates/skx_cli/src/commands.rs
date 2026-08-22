//! Implementations behind each CLI subcommand. Thin orchestration only:
//! all the actual parsing/writing/drift logic lives in `skx_core`, and
//! target-specific compilation lives in `skx_adapters`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use skx_core::{
    ArtifactKind, ArtifactRecord, CompileCtx, DriftStatus, LinkStrategy, Manifest, ManifestEntry,
    Scope, StateFile, artifact_kind_and_sub_key, default_pick, group_by_name, manifest_path,
    scan_for_unmanaged_skills, state_path,
};

pub fn init(root: &Path) -> Result<()> {
    let path = manifest_path(root);
    if path.exists() {
        println!("{} already exists; leaving it untouched.", path.display());
        return Ok(());
    }

    let mut detected = Vec::new();
    if root.join(".claude").is_dir() {
        detected.push("claude_code".to_string());
    }
    if root.join(".cursor").is_dir() {
        detected.push("cursor".to_string());
    }
    if root.join(".github/copilot-instructions.md").is_file() {
        detected.push("copilot".to_string());
    }
    if root.join(".agents").is_dir() {
        detected.push("antigravity".to_string());
    }
    if root.join(".vscode/mcp.json").is_file() {
        detected.push("mcp".to_string());
    }

    let manifest = Manifest {
        detected_targets: detected.clone(),
        skills: Vec::new(),
    };
    manifest.save(&path)?;

    if detected.is_empty() {
        println!(
            "Initialized {} (no existing agent folders detected).",
            path.display()
        );
    } else {
        println!(
            "Initialized {} — detected targets: {}",
            path.display(),
            detected.join(", ")
        );
    }
    Ok(())
}

/// Directories the Agent Skills spec defines alongside `SKILL.md`.
///
/// An allowlist rather than "copy everything": a skill is often authored
/// inside a git checkout, and mirroring the whole directory would drag
/// `.git/`, editor state and build output into the cache.
const BUNDLED_DIRS: &[&str] = &["scripts", "references", "assets"];

/// Copies any spec directories sitting beside a `SKILL.md` into the cache.
/// Returns the names of the ones that existed.
fn copy_bundled_dirs(source_dir: Option<&Path>, dest_dir: &Path) -> Result<Vec<String>> {
    let Some(source_dir) = source_dir else {
        return Ok(Vec::new());
    };
    let mut copied = Vec::new();
    for name in BUNDLED_DIRS {
        let from = source_dir.join(name);
        if !from.is_dir() {
            continue;
        }
        let to = dest_dir.join(name);
        // Replace rather than merge, so a file deleted upstream doesn't
        // linger in the cache and keep resolving.
        if to.exists() {
            std::fs::remove_dir_all(&to)?;
        }
        copy_tree(&from, &to)?;
        copied.push((*name).to_string());
    }
    Ok(copied)
}

fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source).unwrap_or(entry.path());
        let target = dest.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub fn add(root: &Path, home: &Path, source: &str, global: bool) -> Result<()> {
    if source.contains("://") {
        bail!(
            "URL sources aren't supported yet — pass a local path to a SKILL.md \
             file or a directory containing one."
        );
    }

    let source_path = Path::new(source);
    let skill_md_path = if source_path.is_dir() {
        source_path.join("SKILL.md")
    } else {
        source_path.to_path_buf()
    };
    if !skill_md_path.is_file() {
        bail!("no SKILL.md found at {}", skill_md_path.display());
    }

    let skill = skx_core::load_skill(&skill_md_path)?;
    let scope = if global { Scope::Global } else { Scope::Local };
    let dest_dir = skx_core::skill_dir(scope, root, home, skill.frontmatter.name.as_str());
    let dest = dest_dir.join("SKILL.md");
    std::fs::create_dir_all(&dest_dir)?;
    std::fs::copy(&skill_md_path, &dest)?;

    // A skill is a directory: the spec allows `scripts/`, `references/` and
    // `assets/` beside `SKILL.md`, and the body refers to them by relative
    // path. Copying only `SKILL.md` left those behind, so every such
    // reference dangled the moment the skill was linked anywhere other than
    // its original directory.
    let bundled = copy_bundled_dirs(skill_md_path.parent(), &dest_dir)?;
    if !bundled.is_empty() {
        println!("Bundled: {}", bundled.join(", "));
    }

    let path = manifest_path(root);
    let mut manifest = Manifest::load(&path)?;
    manifest.upsert(ManifestEntry {
        name: skill.frontmatter.name.to_string(),
        source: source.to_string(),
        scope,
        version: skill.frontmatter.version.clone(),
    });
    manifest.save(&path)?;

    println!(
        "Added {} v{} ({scope:?} scope) → {}",
        skill.frontmatter.name,
        skill.frontmatter.version,
        dest.display()
    );
    println!("Run `skx sync` to compile it into your agent configs.");
    Ok(())
}

pub fn list(root: &Path) -> Result<()> {
    let manifest = Manifest::load(&manifest_path(root))?;
    if manifest.skills.is_empty() {
        println!("No skills installed. Run `skx add <path>` first.");
        return Ok(());
    }
    for entry in &manifest.skills {
        println!(
            "{} v{} ({:?}) — {}",
            entry.name, entry.version, entry.scope, entry.source
        );
    }
    Ok(())
}

/// The " (would declare X on import)" suffix shown next to a candidate
/// that doesn't yet declare the target matching where it was found —
/// informational only, since this command never actually imports anything.
fn target_hint(skill: &skx_core::DiscoveredSkill) -> String {
    match skill.found_in.default_target_key() {
        Some(key) if !skill.skill.frontmatter.targets.contains_key(key) => {
            format!(" (would declare {key} on import)")
        }
        _ => String::new(),
    }
}

/// Reports `SKILL.md` files that already exist on disk (in the global
/// caches, this workspace's own `.claude/skills`/`.agents/skills`, or any
/// `extra_roots`) but aren't yet tracked by skx — the case of someone
/// who's been hand-managing skills since before installing skx.
///
/// Report-only, on purpose: this never writes to `skx.toml` or copies
/// anything into the cache, so running it in CI or by habit can't
/// silently change what's installed. To actually bring skills in, open
/// `skx tui` and press `d`.
pub fn discover(root: &Path, home: &Path, extra_roots: &[PathBuf]) -> Result<()> {
    let manifest = Manifest::load(&manifest_path(root))?;
    let found = scan_for_unmanaged_skills(&manifest, root, home, extra_roots);

    if found.is_empty() {
        println!("No unmanaged skills found.");
        return Ok(());
    }

    let groups = group_by_name(&found);
    let conflicts = groups.values().filter(|g| g.len() > 1).count();

    for indices in groups.values() {
        if indices.len() == 1 {
            let skill = &found[indices[0]];
            println!(
                "{} v{} ({:?}){} — {}",
                skill.skill.frontmatter.name,
                skill.skill.frontmatter.version,
                skill.scope_hint,
                target_hint(skill),
                skill.path.display()
            );
        } else {
            let pick = default_pick(&found, indices);
            println!(
                "{} — {} candidates found, conflict:",
                found[indices[0]].skill.frontmatter.name,
                indices.len()
            );
            for &i in indices {
                let skill = &found[i];
                let marker = if i == pick { "*" } else { " " };
                println!(
                    "  {marker} v{} ({:?}){} — {}",
                    skill.skill.frontmatter.version,
                    skill.scope_hint,
                    target_hint(skill),
                    skill.path.display()
                );
            }
        }
    }

    println!(
        "\n{} unmanaged skill(s) found ({conflicts} naming conflict(s), marked with * for the default pick).",
        found.len()
    );
    println!("Run `skx tui` and press 'd' to review and import them.");
    Ok(())
}

pub fn sync(root: &Path, home: &Path) -> Result<()> {
    let manifest = Manifest::load(&manifest_path(root))?;
    if manifest.skills.is_empty() {
        println!("No skills installed. Run `skx add <path>` first.");
        return Ok(());
    }

    let mut state = StateFile::load(&state_path(root))?;
    let adapters = skx_adapters::default_adapters();
    let mut written = 0usize;

    for entry in &manifest.skills {
        let cache_file = skx_core::skill_path(entry.scope, root, home, &entry.name);
        let skill = match skx_core::load_skill(&cache_file) {
            Ok(skill) => skill,
            Err(e) => {
                eprintln!("skipping {}: {e}", entry.name);
                continue;
            }
        };

        let ctx = CompileCtx {
            root,
            home,
            scope: entry.scope,
            cache: &skx_core::skill_dir(entry.scope, root, home, &entry.name),
        };
        for adapter in &adapters {
            let output = match adapter.compile(&skill, &ctx) {
                Ok(output) => output,
                Err(e) => {
                    eprintln!("  [{}] {e}", adapter.target_name());
                    continue;
                }
            };
            for artifact in &output.artifacts {
                let cache_source = matches!(adapter.link_strategy(), LinkStrategy::Symlink)
                    .then_some(cache_file.as_path());
                let write = skx_core::apply(artifact, adapter.link_strategy(), cache_source)?;
                let (kind, sub_key) = artifact_kind_and_sub_key(artifact);
                state.upsert(ArtifactRecord {
                    path: artifact.path().to_path_buf(),
                    sub_key,
                    skill: entry.name.clone(),
                    skill_version: skill.frontmatter.version.clone(),
                    target: adapter.target_name().to_string(),
                    kind,
                    content_hash: write.content_hash,
                    symlink_target: write.symlink_target,
                });
                written += 1;

                // skx moved files the user never asked it to move, so it
                // says so. This fires once per skill when migrating a
                // workspace synced by a version that linked SKILL.md alone
                // and left the surrounding directory real.
                for file in &write.adopted {
                    println!(
                        "  adopted {} into the cache for {}",
                        file.display(),
                        entry.name
                    );
                }
            }
        }
    }

    state.save(&state_path(root))?;
    println!(
        "Synced {} skill(s), wrote/linked {written} artifact(s).",
        manifest.skills.len()
    );
    Ok(())
}

/// Compiles every installed skill into standalone static files under
/// `out`, for committing to a team repo or feeding to CI/CD — unlike
/// `sync`, this never depends on `~/.config/skx` or `.skx/` being present
/// wherever the output ends up.
///
/// Two differences from `sync`: output always lands under `out` at
/// `Scope::Local` regardless of a skill's installed scope (a portable
/// bundle has no notion of "global"), and every artifact is force-written
/// as real bytes even for adapters that would normally symlink — a static
/// export can't point a symlink back at a cache that won't exist once the
/// bundle is copied elsewhere. Doesn't touch `.skx/state.toml`; this is a
/// one-shot compile, not a tracked sync.
pub fn export(root: &Path, home: &Path, out: &Path) -> Result<()> {
    let manifest = Manifest::load(&manifest_path(root))?;
    if manifest.skills.is_empty() {
        println!("No skills installed. Run `skx add <path>` first.");
        return Ok(());
    }

    std::fs::create_dir_all(out)?;
    let adapters = skx_adapters::default_adapters();
    let mut written = 0usize;

    for entry in &manifest.skills {
        let cache_file = skx_core::skill_path(entry.scope, root, home, &entry.name);
        let skill = match skx_core::load_skill(&cache_file) {
            Ok(skill) => skill,
            Err(e) => {
                eprintln!("skipping {}: {e}", entry.name);
                continue;
            }
        };

        let ctx = CompileCtx {
            root: out,
            home,
            // Output is redirected into `out`, but the skill itself still
            // lives wherever it was installed — hence the explicit cache.
            scope: Scope::Local,
            cache: &skx_core::skill_dir(entry.scope, root, home, &entry.name),
        };
        for adapter in &adapters {
            let output = match adapter.compile(&skill, &ctx) {
                Ok(output) => output,
                Err(e) => {
                    eprintln!("  [{}] {e}", adapter.target_name());
                    continue;
                }
            };
            for artifact in &output.artifacts {
                skx_core::apply(artifact, LinkStrategy::Compile, None)?;
                written += 1;
            }
        }
    }

    println!(
        "Exported {} skill(s), wrote {written} file(s) to {}",
        manifest.skills.len(),
        out.display()
    );
    Ok(())
}

pub fn remove(root: &Path, home: &Path, name: &str) -> Result<()> {
    let path = manifest_path(root);
    let mut manifest = Manifest::load(&path)?;
    let Some(entry) = manifest.remove(name) else {
        println!("No installed skill named {name}.");
        return Ok(());
    };

    let mut state = StateFile::load(&state_path(root))?;
    let records = state.remove_skill(name);
    for record in &records {
        match record.kind {
            ArtifactKind::OwnedFile => skx_core::remove_owned_file(&record.path)?,
            ArtifactKind::OwnedDir => skx_core::remove_owned_dir(&record.path)?,
            ArtifactKind::Region => skx_core::remove_region(
                &record.path,
                record.sub_key.as_deref().unwrap_or_default(),
            )?,
            ArtifactKind::MergeJson => skx_core::remove_json_pointer(
                &record.path,
                record.sub_key.as_deref().unwrap_or_default(),
            )?,
        }
    }

    let cache_dir = skx_core::cache_dir(entry.scope, root, home).join(&entry.name);
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)?;
    }

    manifest.save(&path)?;
    state.save(&state_path(root))?;
    println!("Removed {name} ({} artifact(s) unlinked).", records.len());
    Ok(())
}

pub fn audit(root: &Path, home: &Path) -> Result<()> {
    let manifest = Manifest::load(&manifest_path(root))?;
    let state = StateFile::load(&state_path(root))?;
    let adapters = skx_adapters::default_adapters();

    let mut seen: HashSet<(PathBuf, Option<String>)> = HashSet::new();
    let mut issues = 0usize;

    for entry in &manifest.skills {
        let cache_file = skx_core::skill_path(entry.scope, root, home, &entry.name);
        let skill = match skx_core::load_skill(&cache_file) {
            Ok(skill) => skill,
            Err(e) => {
                println!("[{}] cache missing or invalid: {e}", entry.name);
                issues += 1;
                continue;
            }
        };

        let ctx = CompileCtx {
            root,
            home,
            scope: entry.scope,
            cache: &skx_core::skill_dir(entry.scope, root, home, &entry.name),
        };
        for adapter in &adapters {
            let Ok(output) = adapter.compile(&skill, &ctx) else {
                continue;
            };
            for artifact in &output.artifacts {
                let (_, sub_key) = artifact_kind_and_sub_key(artifact);
                seen.insert((artifact.path().to_path_buf(), sub_key.clone()));

                let Some(record) = state.record_for(artifact.path(), sub_key.as_deref()) else {
                    println!(
                        "[{} / {}] {} — not yet synced",
                        entry.name,
                        adapter.target_name(),
                        artifact.path().display()
                    );
                    issues += 1;
                    continue;
                };

                let cache_source = matches!(adapter.link_strategy(), LinkStrategy::Symlink)
                    .then_some(cache_file.as_path());
                let fresh = skx_core::fresh_hash(artifact, adapter.link_strategy(), cache_source)?;
                let status = skx_core::audit_record(record, Some(&fresh))?;
                if status != DriftStatus::InSync {
                    issues += 1;
                    println!(
                        "[{} / {}] {} — {status:?}",
                        entry.name,
                        adapter.target_name(),
                        artifact.path().display()
                    );
                }
            }
        }
    }

    for record in &state.artifacts {
        let key = (record.path.clone(), record.sub_key.clone());
        if !seen.contains(&key) {
            issues += 1;
            println!(
                "[{} / {}] {} — orphaned record (not produced by any installed skill)",
                record.skill,
                record.target,
                record.path.display()
            );
        }
    }

    if issues == 0 {
        println!("Everything in sync.");
    } else {
        println!("{issues} issue(s) found.");
    }
    Ok(())
}
