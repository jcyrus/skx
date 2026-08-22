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

/// The canonical directory for `name` at `scope`.
///
/// A skill is a directory, not a file: the spec allows `scripts/`,
/// `references/` and `assets/` alongside `SKILL.md`, and the body refers to
/// them by relative path. This is the unit that gets cached and linked.
pub fn skill_dir(scope: Scope, root: &Path, home: &Path, name: &str) -> PathBuf {
    cache_dir(scope, root, home).join(name)
}

/// The canonical `SKILL.md` path for `name` at `scope`.
pub fn skill_path(scope: Scope, root: &Path, home: &Path, name: &str) -> PathBuf {
    skill_dir(scope, root, home, name).join("SKILL.md")
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

/// Environment variable that relocates everything `skx` keeps in the home
/// directory — the global cache and `config.toml`.
pub const HOME_OVERRIDE: &str = "SKX_HOME";

/// The directory `skx` treats as home.
///
/// `SKX_HOME` wins when set. That override isn't a test affordance bolted
/// on afterwards — `dirs::home_dir()` resolves via `SHGetKnownFolderPath`
/// on Windows and so cannot be redirected by `HOME` or `USERPROFILE` at
/// all, which left the global-scope integration tests writing skills into
/// the real user profile there. Anything that can't be pointed somewhere
/// else also can't be sandboxed, by a test or by a user who wants their
/// config on another volume.
pub fn home_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os(HOME_OVERRIDE) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => dirs::home_dir(),
    }
}

#[cfg(test)]
mod home_tests {
    use super::*;

    /// Serialised because they mutate process-wide environment state.
    #[test]
    fn the_override_wins_and_an_empty_value_is_ignored() {
        // SAFETY: single-threaded test, and the variable is restored below.
        unsafe {
            std::env::set_var(HOME_OVERRIDE, "/tmp/skx-home");
        }
        assert_eq!(home_dir(), Some(PathBuf::from("/tmp/skx-home")));

        unsafe {
            std::env::set_var(HOME_OVERRIDE, "");
        }
        assert_eq!(
            home_dir(),
            dirs::home_dir(),
            "an empty override must not resolve home to nothing"
        );

        unsafe {
            std::env::remove_var(HOME_OVERRIDE);
        }
        assert_eq!(home_dir(), dirs::home_dir());
    }
}
