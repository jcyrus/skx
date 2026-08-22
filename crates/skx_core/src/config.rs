//! User preferences, persisted at `~/.config/skx/config.toml`.
//!
//! Deliberately separate from [`crate::manifest::Manifest`]: the manifest
//! describes *this workspace's* skills and belongs in version control,
//! while config describes *this person's* preferences and belongs to the
//! machine. Committing one and not the other is the whole reason they're
//! different files.
//!
//! Every field is optional and every unknown key is preserved, so a config
//! written by a newer `skx` still loads on an older one without losing the
//! settings it doesn't understand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SkillError};

/// Where preferences live, given a home directory.
pub fn config_path(home: &Path) -> PathBuf {
    home.join(".config/skx/config.toml")
}

/// Which palette the cockpit should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    Light,
    Dark,
    /// Detect from the environment, falling back to dark.
    #[default]
    Auto,
}

impl ThemePreference {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// The string [`Palette::resolve`](../../skx_tui/theme/struct.Palette.html)
    /// expects, or `None` for "work it out from the environment".
    pub fn as_explicit(self) -> Option<&'static str> {
        match self {
            Self::Light => Some("light"),
            Self::Dark => Some("dark"),
            Self::Auto => None,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemePreference,

    /// Ask before quitting even when nothing is pending.
    ///
    /// A confirmation the user always dismisses stops being read, so this
    /// is only about the *quiet* case: `skx` always asks when there are
    /// unsynced changes or a sync in flight, regardless of this setting.
    #[serde(default = "yes")]
    pub confirm_quit: bool,

    /// Respond to mouse clicks and the scroll wheel.
    #[serde(default = "yes")]
    pub mouse: bool,

    /// Set the terminal window/tab title while running.
    #[serde(default = "yes")]
    pub set_terminal_title: bool,

    /// Keys the running version doesn't know about, kept so a config
    /// written by a newer `skx` survives a round-trip through an older one.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemePreference::default(),
            confirm_quit: true,
            mouse: true,
            set_terminal_title: true,
            extra: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Loads preferences, or returns defaults when the file doesn't exist.
    ///
    /// A malformed config is an error rather than a silent fallback:
    /// quietly ignoring a typo'd setting and behaving differently than the
    /// file says is worse than refusing to start with a message.
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

    /// Applies a `--theme` flag over whatever the config says. The flag is
    /// a one-run override and is never written back.
    pub fn with_theme_override(mut self, flag: Option<&str>) -> Self {
        if let Some(preference) = flag.and_then(ThemePreference::parse) {
            self.theme = preference;
        }
        self
    }

    /// Whether colour should be emitted at all.
    ///
    /// Follows the `NO_COLOR` informal standard: any non-empty value
    /// disables colour, regardless of its content.
    pub fn color_enabled() -> bool {
        !std::env::var("NO_COLOR").is_ok_and(|v| !v.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(raw: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, raw).unwrap();
        (dir, path)
    }

    #[test]
    fn a_missing_config_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(config, Config::default());
        assert!(config.confirm_quit, "quit confirmation ships enabled");
        assert!(config.mouse);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        let config = Config {
            theme: ThemePreference::Light,
            confirm_quit: false,
            ..Config::default()
        };
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn a_partial_config_keeps_defaults_for_everything_else() {
        let (_dir, path) = write("theme = \"dark\"\n");
        let config = Config::load(&path).unwrap();
        assert_eq!(config.theme, ThemePreference::Dark);
        assert!(config.confirm_quit);
        assert!(config.mouse);
    }

    /// A config written by a newer `skx` must survive an older one.
    #[test]
    fn unknown_keys_are_preserved_rather_than_dropped() {
        let (_dir, path) = write("theme = \"light\"\nfuture_setting = 42\n");
        let config = Config::load(&path).unwrap();
        assert!(config.extra.contains_key("future_setting"));

        let out = path.parent().unwrap().join("out.toml");
        config.save(&out).unwrap();
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .contains("future_setting")
        );
    }

    /// Silently ignoring a typo and behaving differently than the file says
    /// is worse than refusing to start.
    #[test]
    fn a_malformed_config_is_an_error_not_a_silent_fallback() {
        let (_dir, path) = write("theme = = broken\n");
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn the_theme_flag_overrides_the_config() {
        let config = Config {
            theme: ThemePreference::Dark,
            ..Config::default()
        };
        assert_eq!(
            config.clone().with_theme_override(Some("light")).theme,
            ThemePreference::Light
        );
        // An absent or unparseable flag leaves the config alone.
        assert_eq!(
            config.clone().with_theme_override(None).theme,
            ThemePreference::Dark
        );
        assert_eq!(
            config.with_theme_override(Some("chartreuse")).theme,
            ThemePreference::Dark
        );
    }

    #[test]
    fn theme_preference_maps_to_an_explicit_palette_or_defers() {
        assert_eq!(ThemePreference::Light.as_explicit(), Some("light"));
        assert_eq!(ThemePreference::Dark.as_explicit(), Some("dark"));
        assert_eq!(ThemePreference::Auto.as_explicit(), None);
    }
}
