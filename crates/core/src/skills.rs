use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::SkillsConfig;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Skills {
    pub skills: Vec<Skill>,
}

impl Skills {
    pub fn load(workspace_root: &Path, config: &SkillsConfig) -> Skills {
        let mut skills: Vec<Skill> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        let mut dirs: Vec<PathBuf> = Vec::new();
        dirs.push(workspace_root.join(".agents").join("skills"));
        if let Ok(global) = crate::config::config_dir() {
            dirs.push(global.join("skills"));
        }
        for d in &config.dirs {
            dirs.push(PathBuf::from(d));
        }

        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            for path in paths {
                if !path.is_dir() {
                    continue;
                }
                if let Some(skill) = Skill::from_dir(&path)
                    && seen.insert(skill.name.clone())
                {
                    skills.push(skill);
                }
            }
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Skills { skills }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn preamble_section(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut s = String::from(
            "Available skills are reference docs you can consult by reading their SKILL.md file \
             (via read_file) when a task matches their domain:",
        );
        for skill in &self.skills {
            s.push_str(&format!(
                "\n- {}: {} (path: {})",
                skill.name,
                skill.description,
                skill.path.display()
            ));
        }
        Some(s)
    }
}

impl Skill {
    fn from_dir(dir: &Path) -> Option<Skill> {
        let skill_md = dir.join("SKILL.md");
        if !skill_md.is_file() {
            return None;
        }
        let content = std::fs::read_to_string(&skill_md).ok()?;
        let fm = parse_front_matter(&content)?;
        let name = match fm.name {
            Some(n) => n,
            None => dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        Some(Skill {
            name,
            description: fm.description.unwrap_or_default(),
            tags: fm.metadata.tags,
            category: fm.metadata.category,
            path: dir.to_path_buf(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct FrontMatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    metadata: FrontMatterMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct FrontMatterMetadata {
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

fn parse_front_matter(content: &str) -> Option<FrontMatter> {
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let yaml = &rest[..end];
    serde_yaml::from_str(yaml).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(root: &Path, name: &str, description: &str, tags: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let tags = tags
            .iter()
            .map(|t| format!("    - {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: >-\n  {description}\nlicense: MIT\nmetadata:\n  \
                 category: Test\n  tags:\n{tags}\n---\n\n# {name}\n\nbody\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn parses_front_matter() {
        let content = "---\nname: ratatui\ndescription: >-\n  A TUI skill\nmetadata:\n  \
                       category: Frontend\n  tags:\n    - rust\n    - tui\n---\n\n# body\n";
        let fm = parse_front_matter(content).unwrap();
        assert_eq!(fm.name.as_deref(), Some("ratatui"));
        assert_eq!(fm.description.as_deref(), Some("A TUI skill"));
        assert_eq!(fm.metadata.category.as_deref(), Some("Frontend"));
        assert_eq!(fm.metadata.tags, vec!["rust", "tui"]);
    }

    #[test]
    fn loads_workspace_and_global_skills() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let global = dir.path().join("global");
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(global.join("skills")).unwrap();
        write_skill(
            &workspace.join(".agents/skills"),
            "ratatui",
            "TUI skill",
            &["rust"],
        );
        write_skill(&global.join("skills"), "tokio", "Async skill", &["rust"]);

        let config = SkillsConfig {
            disabled: false,
            dirs: vec![global.join("skills").to_string_lossy().into_owned()],
        };
        let skills = Skills::load(&workspace, &config);
        let names: Vec<&str> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["ratatui", "tokio"]);
        assert_eq!(skills.skills[0].tags, vec!["rust"]);
        assert_eq!(skills.skills[1].category.as_deref(), Some("Test"));
    }

    #[test]
    fn dedupes_by_name_workspace_wins() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let global = dir.path().join("global");
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();
        std::fs::create_dir_all(global.join("skills")).unwrap();
        write_skill(
            &workspace.join(".agents/skills"),
            "ratatui",
            "workspace copy",
            &[],
        );
        write_skill(&global.join("skills"), "ratatui", "global copy", &[]);

        let config = SkillsConfig {
            disabled: false,
            dirs: vec![global.join("skills").to_string_lossy().into_owned()],
        };
        let skills = Skills::load(&workspace, &config);
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].description, "workspace copy");
    }

    #[test]
    fn preamble_section_lists_skills() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();
        write_skill(
            &workspace.join(".agents/skills"),
            "ratatui",
            "TUI skill",
            &["rust"],
        );
        let skills = Skills::load(&workspace, &SkillsConfig::default());
        let section = skills.preamble_section().unwrap();
        assert!(section.contains("ratatui"));
        assert!(section.contains("TUI skill"));
        assert!(section.contains("SKILL.md"));
    }

    #[test]
    fn empty_when_no_skills() {
        let dir = TempDir::new().unwrap();
        let skills = Skills::load(dir.path(), &SkillsConfig::default());
        assert!(skills.is_empty());
        assert!(skills.preamble_section().is_none());
    }
}
