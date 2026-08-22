use std::path::{Path, PathBuf};

use crate::error::{Result, SkillError};
use crate::model::{Frontmatter, Skill};

const DELIMITER: &str = "---";

/// Splits a raw `SKILL.md` document into its YAML frontmatter block and
/// markdown body. Frontmatter must start on the first non-empty line with
/// a bare `---` and end with a matching `---` on its own line.
pub fn split_frontmatter<'a>(source: &'a str, path: &Path) -> Result<(&'a str, &'a str)> {
    let trimmed_start = source.trim_start_matches('\u{feff}');
    let mut lines = trimmed_start.split_inclusive('\n');

    let first_line = lines
        .next()
        .ok_or_else(|| SkillError::MissingFrontmatter(path.to_path_buf()))?;
    if first_line.trim_end() != DELIMITER {
        return Err(SkillError::MissingFrontmatter(path.to_path_buf()));
    }

    let after_first_delim = &trimmed_start[first_line.len()..];
    let mut offset = 0usize;
    for line in after_first_delim.split_inclusive('\n') {
        if line.trim_end() == DELIMITER {
            let frontmatter = &after_first_delim[..offset];
            let body_start = offset + line.len();
            let body = after_first_delim[body_start..].trim_start_matches('\n');
            return Ok((frontmatter, body));
        }
        offset += line.len();
    }

    Err(SkillError::UnterminatedFrontmatter(path.to_path_buf()))
}

/// Parses a raw `SKILL.md` document (frontmatter + body) into a [`Skill`].
pub fn parse_skill(source: &str, path: &Path) -> Result<Skill> {
    let (frontmatter_yaml, body) = split_frontmatter(source, path)?;
    let frontmatter: Frontmatter = serde_yaml::from_str(frontmatter_yaml).map_err(|source| {
        SkillError::InvalidFrontmatter {
            path: path.to_path_buf(),
            source,
        }
    })?;

    Ok(Skill {
        frontmatter,
        body: body.to_string(),
        source_path: Some(path.to_path_buf()),
    })
}

/// Reads and parses a `SKILL.md` file from disk.
pub fn load_skill(path: &Path) -> Result<Skill> {
    let source = std::fs::read_to_string(path).map_err(|source| SkillError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_skill(&source, path)
}

/// Serializes a [`Skill`] back into canonical `SKILL.md` text
/// (YAML frontmatter delimited by `---`, followed by the markdown body).
pub fn render_skill(skill: &Skill) -> Result<String> {
    let yaml =
        serde_yaml::to_string(&skill.frontmatter).map_err(|source| SkillError::Serialize {
            name: skill.frontmatter.name.to_string(),
            source,
        })?;
    Ok(format!("---\n{yaml}---\n\n{}", skill.body))
}

/// Discovers every `SKILL.md` under `root`, recursively.
pub fn discover_skills(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|source| SkillError::Walk {
            path: root.to_path_buf(),
            source,
        })?;
        if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
            found.push(entry.into_path());
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "---\nname: minimal\ndescription: a minimal skill\n---\nbody text\n";

    #[test]
    fn splits_minimal_frontmatter() {
        let path = Path::new("SKILL.md");
        let (frontmatter, body) = split_frontmatter(MINIMAL, path).unwrap();
        assert_eq!(frontmatter, "name: minimal\ndescription: a minimal skill\n");
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn missing_frontmatter_errors() {
        let path = Path::new("SKILL.md");
        let err = split_frontmatter("no delimiter here\n", path).unwrap_err();
        assert!(matches!(err, SkillError::MissingFrontmatter(_)));
    }

    #[test]
    fn unterminated_frontmatter_errors() {
        let path = Path::new("SKILL.md");
        let err = split_frontmatter("---\nname: x\n", path).unwrap_err();
        assert!(matches!(err, SkillError::UnterminatedFrontmatter(_)));
    }

    #[test]
    fn parse_skill_applies_defaults() {
        let path = Path::new("SKILL.md");
        let skill = parse_skill(MINIMAL, path).unwrap();
        assert_eq!(skill.frontmatter.name, "minimal");
        assert_eq!(skill.frontmatter.version, "0.1.0");
        assert!(skill.frontmatter.triggers.is_empty());
        assert!(skill.frontmatter.targets.is_empty());
        assert!(skill.frontmatter.mcp_dependencies.is_empty());
        assert_eq!(skill.body, "body text\n");
    }

    #[test]
    fn invalid_yaml_errors() {
        let path = Path::new("SKILL.md");
        let src = "---\nname: [unterminated\n---\nbody\n";
        let err = parse_skill(src, path).unwrap_err();
        assert!(matches!(err, SkillError::InvalidFrontmatter { .. }));
    }
}
