//! Finds `SKILL.md` files that already exist on disk but aren't yet
//! tracked by skx — the case of a user who's been hand-managing Claude
//! Code / Antigravity skills for a while before installing skx, and wants
//! everything they already have folded into one manifest instead of
//! running `skx add` by hand for each one.
//!
//! Only genuine `SKILL.md` files are in scope. Cursor's `.mdc` rules and
//! Copilot's `copilot-instructions.md` aren't canonical format — importing
//! those would mean guessing at a `name`/`version` that was never there,
//! which is exactly the kind of lossy reverse-compile this project has
//! deliberately stayed out of (see the README: no bi-directional sync).

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache::cache_dir;
use crate::manifest::Manifest;
use crate::model::Scope;
use crate::model::Skill;
use crate::parser::{discover_skills, load_skill};

/// Which agent's own directory a candidate was found in, if any. Lets an
/// importer keep the skill working exactly where it was already being
/// read from directly, without guessing at a target for paths that don't
/// match a known layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundIn {
    ClaudeCode,
    Antigravity,
    /// Found via an explicitly-passed extra root that isn't a recognized
    /// agent skills directory — e.g. a `SKILL.md` sitting loose in some
    /// other layout. Nothing to infer a target from.
    Other,
}

impl FoundIn {
    /// The `targets` key to auto-declare on import if the skill doesn't
    /// already have one — `None` for [`FoundIn::Other`], since guessing a
    /// target for an unrecognized layout would be exactly the kind of
    /// unfounded inference this project avoids elsewhere (see
    /// `skx_adapters`' opt-in-only compilation rule).
    pub fn default_target_key(self) -> Option<&'static str> {
        match self {
            FoundIn::ClaudeCode => Some("claude_code"),
            FoundIn::Antigravity => Some("antigravity"),
            FoundIn::Other => None,
        }
    }
}

fn classify_found_in(path: &Path) -> FoundIn {
    let Some(agent_dir) = path.parent().and_then(Path::parent) else {
        return FoundIn::Other;
    };
    if agent_dir.ends_with(".claude/skills") {
        FoundIn::ClaudeCode
    } else if agent_dir.ends_with(".agents/skills") || agent_dir.ends_with(".gemini/config/skills")
    {
        FoundIn::Antigravity
    } else {
        FoundIn::Other
    }
}

/// A `SKILL.md` found somewhere skx doesn't yet track.
#[derive(Debug, Clone)]
pub struct DiscoveredSkill {
    pub path: PathBuf,
    /// Global if found under a known global directory (`~/.claude/skills`,
    /// `~/.gemini/config/skills`) or an explicitly-passed extra root;
    /// Local if found under the current workspace's own `.claude/skills`
    /// or `.agents/skills`. This is a hint for where to register the
    /// import, not a guarantee — a reviewer can still override it.
    pub scope_hint: Scope,
    /// Which agent directory this was found in, if a recognized one —
    /// drives the default-target-on-import behavior. See
    /// [`FoundIn::default_target_key`].
    pub found_in: FoundIn,
    pub skill: Skill,
}

/// Scans the well-known skill directories (global caches plus this
/// workspace's own `.claude/skills`/`.agents/skills`) and any
/// `extra_roots`, returning every `SKILL.md` that isn't already skx-managed
/// (a symlink pointing into skx's own cache) or already registered in
/// `manifest` (matched by name).
///
/// Deliberately bounded by default: this never walks the whole home
/// directory. Callers that want to check other project directories pass
/// them explicitly via `extra_roots`.
pub fn scan_for_unmanaged_skills(
    manifest: &Manifest,
    root: &Path,
    home: &Path,
    extra_roots: &[PathBuf],
) -> Vec<DiscoveredSkill> {
    let mut roots: Vec<(PathBuf, Scope)> = vec![
        (home.join(".claude/skills"), Scope::Global),
        (home.join(".gemini/config/skills"), Scope::Global),
        (root.join(".claude/skills"), Scope::Local),
        (root.join(".agents/skills"), Scope::Local),
    ];
    for extra in extra_roots {
        roots.push((extra.clone(), Scope::Local));
    }

    let skx_caches = [
        cache_dir(Scope::Global, root, home),
        cache_dir(Scope::Local, root, home),
    ]
    .map(|p| p.canonicalize().unwrap_or(p));

    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();

    for (dir, scope_hint) in roots {
        if !dir.is_dir() {
            continue;
        }
        let Ok(paths) = discover_skills(&dir) else {
            continue;
        };
        for path in paths {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if !seen.insert(canonical.clone()) {
                continue; // already found via another root
            }
            if skx_caches.iter().any(|cache| canonical.starts_with(cache)) {
                continue; // already skx-managed (symlinked into our own cache)
            }
            let Ok(skill) = load_skill(&path) else {
                continue; // unparseable — not our job to fix it here
            };
            if manifest.get(skill.frontmatter.name.as_str()).is_some() {
                continue; // already registered
            }
            let found_in = classify_found_in(&path);
            found.push(DiscoveredSkill {
                path,
                scope_hint,
                found_in,
                skill,
            });
        }
    }

    found
}

/// Groups `candidates` by skill name, preserving discovery order within
/// each group. A group with more than one entry means the same skill name
/// was found in more than one place — a naming conflict the caller needs
/// to resolve (see [`default_pick`]).
pub fn group_by_name(candidates: &[DiscoveredSkill]) -> BTreeMap<String, Vec<usize>> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, candidate) in candidates.iter().enumerate() {
        groups
            .entry(candidate.skill.frontmatter.name.to_string())
            .or_default()
            .push(i);
    }
    groups
}

/// The index (into `candidates`) that should be preselected within a
/// conflict `group`: the highest declared `version`, tie-broken by
/// lexicographically smallest path so the pick is deterministic rather
/// than depending on filesystem walk order.
pub fn default_pick(candidates: &[DiscoveredSkill], group: &[usize]) -> usize {
    *group
        .iter()
        .min_by_key(|&&i| {
            (
                Reverse(version_key(&candidates[i].skill.frontmatter.version)),
                candidates[i].path.clone(),
            )
        })
        .expect("group_by_name never produces an empty group")
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestEntry;

    const SKILL_TEMPLATE: &str =
        "---\nname: {name}\ndescription: d\nversion: {version}\n---\nbody\n";

    fn write_skill(path: &Path, name: &str, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = SKILL_TEMPLATE
            .replace("{name}", name)
            .replace("{version}", version);
        std::fs::write(path, content).unwrap();
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("workspace");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Fixture {
            _dir: dir,
            root,
            home,
        }
    }

    #[test]
    fn finds_a_hand_installed_local_skill() {
        let fx = fixture();
        write_skill(
            &fx.root.join(".claude/skills/old-skill/SKILL.md"),
            "old-skill",
            "1.0.0",
        );

        let found = scan_for_unmanaged_skills(&Manifest::default(), &fx.root, &fx.home, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].skill.frontmatter.name, "old-skill");
        assert_eq!(found[0].scope_hint, Scope::Local);
        assert_eq!(found[0].found_in, FoundIn::ClaudeCode);
    }

    #[test]
    fn finds_a_hand_installed_global_skill() {
        let fx = fixture();
        write_skill(
            &fx.home.join(".claude/skills/global-skill/SKILL.md"),
            "global-skill",
            "1.0.0",
        );

        let found = scan_for_unmanaged_skills(&Manifest::default(), &fx.root, &fx.home, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].scope_hint, Scope::Global);
        assert_eq!(found[0].found_in, FoundIn::ClaudeCode);
    }

    #[test]
    fn finds_a_hand_installed_antigravity_skill() {
        let fx = fixture();
        write_skill(
            &fx.root.join(".agents/skills/agy-skill/SKILL.md"),
            "agy-skill",
            "1.0.0",
        );

        let found = scan_for_unmanaged_skills(&Manifest::default(), &fx.root, &fx.home, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].found_in, FoundIn::Antigravity);
    }

    #[test]
    fn a_skill_in_an_unrecognized_layout_gets_no_default_target() {
        let fx = fixture();
        let elsewhere = tempfile::tempdir().unwrap();
        write_skill(
            &elsewhere.path().join("random-notes/SKILL.md"),
            "loose-skill",
            "1.0.0",
        );

        let found = scan_for_unmanaged_skills(
            &Manifest::default(),
            &fx.root,
            &fx.home,
            std::slice::from_ref(&elsewhere.path().to_path_buf()),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].found_in, FoundIn::Other);
        assert_eq!(found[0].found_in.default_target_key(), None);
    }

    #[test]
    fn skips_a_skill_already_symlinked_into_the_skx_cache() {
        let fx = fixture();
        let cache_file = fx.home.join(".config/skx/skills/managed-skill/SKILL.md");
        write_skill(&cache_file, "managed-skill", "1.0.0");

        let link_path = fx.root.join(".claude/skills/managed-skill/SKILL.md");
        std::fs::create_dir_all(link_path.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&cache_file, &link_path).unwrap();

        let found = scan_for_unmanaged_skills(&Manifest::default(), &fx.root, &fx.home, &[]);
        assert!(found.is_empty(), "found: {found:?}");
    }

    #[test]
    fn skips_a_skill_already_registered_in_the_manifest() {
        let fx = fixture();
        write_skill(
            &fx.root.join(".claude/skills/tracked/SKILL.md"),
            "tracked",
            "1.0.0",
        );

        let mut manifest = Manifest::default();
        manifest.upsert(ManifestEntry {
            name: "tracked".to_string(),
            source: "/wherever".to_string(),
            scope: Scope::Local,
            version: "1.0.0".to_string(),
        });

        let found = scan_for_unmanaged_skills(&manifest, &fx.root, &fx.home, &[]);
        assert!(found.is_empty(), "found: {found:?}");
    }

    #[test]
    fn extra_roots_are_scanned_too() {
        let fx = fixture();
        let elsewhere = tempfile::tempdir().unwrap();
        write_skill(
            &elsewhere
                .path()
                .join("some-project/.claude/skills/far-away/SKILL.md"),
            "far-away",
            "1.0.0",
        );

        let found = scan_for_unmanaged_skills(
            &Manifest::default(),
            &fx.root,
            &fx.home,
            &[elsewhere.path().to_path_buf()],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].skill.frontmatter.name, "far-away");
    }

    #[test]
    fn does_not_double_count_a_path_found_via_two_roots() {
        let fx = fixture();
        write_skill(&fx.root.join(".claude/skills/dup/SKILL.md"), "dup", "1.0.0");

        // Passing the workspace root itself as an extra root means the
        // same file is reachable via both the built-in `.claude/skills`
        // check and the extra-roots walk.
        let found = scan_for_unmanaged_skills(
            &Manifest::default(),
            &fx.root,
            &fx.home,
            std::slice::from_ref(&fx.root),
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn groups_same_name_candidates_as_a_conflict() {
        let candidates = vec![
            DiscoveredSkill {
                path: PathBuf::from("/a/SKILL.md"),
                scope_hint: Scope::Local,
                found_in: FoundIn::Other,
                skill: parse(
                    SKILL_TEMPLATE
                        .replace("{name}", "dup")
                        .replace("{version}", "1.0.0"),
                ),
            },
            DiscoveredSkill {
                path: PathBuf::from("/b/SKILL.md"),
                scope_hint: Scope::Local,
                found_in: FoundIn::Other,
                skill: parse(
                    SKILL_TEMPLATE
                        .replace("{name}", "dup")
                        .replace("{version}", "2.0.0"),
                ),
            },
            DiscoveredSkill {
                path: PathBuf::from("/c/SKILL.md"),
                scope_hint: Scope::Local,
                found_in: FoundIn::Other,
                skill: parse(
                    SKILL_TEMPLATE
                        .replace("{name}", "solo")
                        .replace("{version}", "1.0.0"),
                ),
            },
        ];

        let groups = group_by_name(&candidates);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["dup"], vec![0, 1]);
        assert_eq!(groups["solo"], vec![2]);

        let pick = default_pick(&candidates, &groups["dup"]);
        assert_eq!(pick, 1, "should prefer version 2.0.0 over 1.0.0");
    }

    fn parse(src: String) -> Skill {
        crate::parser::parse_skill(&src, Path::new("SKILL.md")).unwrap()
    }
}
