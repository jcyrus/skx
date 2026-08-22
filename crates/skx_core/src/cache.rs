//! Resolves where a skill's canonical copy lives on disk: the central
//! global cache (`~/.config/skx/skills/<name>/`) or a workspace-local
//! cache (`.skx/skills/<name>/`). Every adapter's `Symlink` strategy
//! points back at whichever of these holds the skill.

use std::path::{Path, PathBuf};

use crate::Scope;

/// The directory holding every cached skill for `scope`.
pub fn cache_dir(scope: Scope, root: &Path, home: &Path) -> PathBuf {
    match scope {
        Scope::Local => root.join(".skx/skills"),
        Scope::Global => home.join(".config/skx/skills"),
    }
}

/// The canonical `SKILL.md` path for `name` at `scope`.
pub fn skill_path(scope: Scope, root: &Path, home: &Path, name: &str) -> PathBuf {
    cache_dir(scope, root, home).join(name).join("SKILL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_scope_uses_workspace_dot_skx() {
        let path = skill_path(
            Scope::Local,
            Path::new("/workspace"),
            Path::new("/home/user"),
            "foo",
        );
        assert_eq!(path, Path::new("/workspace/.skx/skills/foo/SKILL.md"));
    }

    #[test]
    fn global_scope_uses_home_config() {
        let path = skill_path(
            Scope::Global,
            Path::new("/workspace"),
            Path::new("/home/user"),
            "foo",
        );
        assert_eq!(
            path,
            Path::new("/home/user/.config/skx/skills/foo/SKILL.md")
        );
    }
}
