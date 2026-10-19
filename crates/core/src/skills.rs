use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use shuvarie_config::{Category, SkillsConfig, TrustGrants};

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub path: PathBuf,
    /// Outside the workspace (a user-level global skills dir).
    pub global: bool,
    /// Hidden from the agent preamble; still invocable via `/skill:name`.
    pub disable_model_invocation: bool,
    /// The body of a skill that ships with shuvarie rather than living in a
    /// directory. `path` then points at nothing, because a built-in has no
    /// reference files and no `SKILL.md` on disk: the body *is* the file.
    pub builtin: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct SkillWarning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct Skills {
    pub skills: Vec<Skill>,
    pub warnings: Vec<SkillWarning>,
}

impl Skills {
    /// Loads the skill roster; the workspace `.agents/skills` tree only
    /// loads when the `skills` trust category is granted (global and
    /// config-listed dirs are user-owned and always load).
    pub fn load(workspace_root: &Path, config: &SkillsConfig, trust: &TrustGrants) -> Skills {
        if config.disabled {
            return Skills::default();
        }
        let skills = Self::load_from(
            workspace_root,
            dirs::home_dir().as_deref(),
            shuvarie_config::config_dir().ok().as_deref(),
            config,
        );
        if trust.allows(Category::Skills) {
            return skills;
        }
        let workspace_dir = workspace_root.join(".agents").join("skills");
        Skills {
            skills: skills
                .skills
                .into_iter()
                .filter(|skill| !skill.path.starts_with(&workspace_dir))
                .collect(),
            warnings: skills
                .warnings
                .into_iter()
                .filter(|warning| !warning.path.starts_with(&workspace_dir))
                .collect(),
        }
    }

    /// The skills that ship with shuvarie, added to whatever the workspace and
    /// the user already provide.
    ///
    /// There is one so far, and it is conditional: the `codemode` reference
    /// teaches a language, which is only worth offering when the tool that runs
    /// it is offered too. A user's own skill of the same name wins, so the
    /// reference can be replaced without forking.
    pub fn with_builtins(mut self, codemode: bool) -> Self {
        use shuvarie_codemode::skill;

        if codemode && !self.skills.iter().any(|skill| skill.name == skill::NAME) {
            self.skills.push(Skill {
                name: skill::NAME.to_string(),
                description: skill::DESCRIPTION.to_string(),
                tags: vec!["codemode".to_string(), "rhai".to_string()],
                category: Some("Built-in".to_string()),
                path: PathBuf::new(),
                global: false,
                disable_model_invocation: false,
                builtin: Some(skill::BODY),
            });
        }
        self
    }

    /// The walk behind [`Skills::load`], with injectable global locations so
    /// tests stay hermetic.
    fn load_from(
        workspace_root: &Path,
        home: Option<&Path>,
        global_config: Option<&Path>,
        config: &SkillsConfig,
    ) -> Skills {
        let mut skills: Vec<Skill> = Vec::new();
        let mut warnings: Vec<SkillWarning> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut loaded_files: HashSet<PathBuf> = HashSet::new();

        let mut dirs: Vec<PathBuf> = Vec::new();
        dirs.push(workspace_root.join(".agents").join("skills"));
        dirs.extend(global_skill_dirs(home, global_config));
        for d in &config.dirs {
            dirs.push(PathBuf::from(d));
        }

        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            collect_skills(
                &dir,
                &mut visited,
                &mut loaded_files,
                &mut skills,
                &mut warnings,
                &mut seen,
            );
        }

        let root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        for skill in &mut skills {
            skill.global = !skill.path.starts_with(&root);
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Skills { skills, warnings }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn preamble_section(&self) -> Option<String> {
        let visible: Vec<&Skill> = self
            .skills
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .collect();
        if visible.is_empty() {
            return None;
        }
        let mut s = String::from(
            "The following skills provide specialized instructions for specific tasks. \
             When a task matches a skill's description, load that skill with the `skill` tool \
             (passing the skill name) before starting work, then follow its instructions. \
             A skill's SKILL.md may reference further files relative to its directory — load \
             those with the `skill` tool's `path` argument, and when another tool needs a path \
             from a skill reference, resolve it against the skill directory (the parent of \
             SKILL.md).",
        );
        s.push_str("\n\n<available_skills>");
        for skill in visible {
            s.push_str("\n  <skill>");
            s.push_str(&format!("\n    <name>{}</name>", escape_xml(&skill.name)));
            s.push_str(&format!(
                "\n    <description>{}</description>",
                escape_xml(&skill.description)
            ));
            s.push_str(&format!(
                "\n    <location>{}</location>",
                escape_xml(&skill.location())
            ));
            s.push_str("\n  </skill>");
        }
        s.push_str("\n</available_skills>");
        Some(s)
    }
}

/// Skill locations outside the workspace, in precedence order: the app
/// config's `skills` dir, then the cross-harness `~/.agents/skills`.
pub(crate) fn global_skill_dirs(home: Option<&Path>, global_config: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(global) = global_config {
        dirs.push(global.join("skills"));
    }
    if let Some(home) = home {
        dirs.push(home.join(".agents").join("skills"));
    }
    dirs
}

/// Walk a skill directory. A directory containing `SKILL.md` is a skill root
/// and is not recursed into further; otherwise subdirectories are searched.
/// Symlinked directories that were already visited are skipped.
fn collect_skills(
    dir: &Path,
    visited: &mut HashSet<PathBuf>,
    loaded_files: &mut HashSet<PathBuf>,
    skills: &mut Vec<Skill>,
    warnings: &mut Vec<SkillWarning>,
    seen: &mut HashSet<String>,
) {
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }
    let skill_md = dir.join("SKILL.md");
    if skill_md.is_file() {
        let Ok(canonical_md) = std::fs::canonicalize(&skill_md) else {
            return;
        };
        if !loaded_files.insert(canonical_md) {
            return;
        }
        let (skill, messages) = Skill::from_dir(dir);
        for message in messages {
            warnings.push(SkillWarning {
                path: skill_md.clone(),
                message,
            });
        }
        if let Some(skill) = skill {
            if seen.insert(skill.name.clone()) {
                skills.push(skill);
            } else {
                warnings.push(SkillWarning {
                    path: skill_md,
                    message: format!("duplicate skill name \"{}\" ignored", skill.name),
                });
            }
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        collect_skills(&path, visited, loaded_files, skills, warnings, seen);
    }
}

impl Skill {
    /// Where the skill comes from, for a prompt that has to say where to look
    /// next: a directory for a skill on disk, and nothing to look in when the
    /// skill shipped with shuvarie.
    pub fn location(&self) -> String {
        match self.builtin {
            Some(_) => "built in".to_string(),
            None => self.path.display().to_string(),
        }
    }

    /// Load the skill rooted at `dir`; returns it plus validation messages.
    /// `None` means the skill is not loaded (unreadable, unparsable, or no
    /// description) — every failure still yields a message.
    fn from_dir(dir: &Path) -> (Option<Skill>, Vec<String>) {
        let mut messages: Vec<String> = Vec::new();
        let skill_md = dir.join("SKILL.md");
        let Ok(content) = std::fs::read_to_string(&skill_md) else {
            messages.push("failed to read SKILL.md".to_string());
            return (None, messages);
        };
        let Some(fm) = parse_front_matter(&content) else {
            messages.push("failed to parse SKILL.md frontmatter".to_string());
            return (None, messages);
        };
        let name = match fm.name {
            Some(n) => n,
            None => dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        for error in validate_name(&name) {
            messages.push(error);
        }
        let description = fm.description.unwrap_or_default();
        for error in validate_description(&description) {
            messages.push(error);
        }
        if description.trim().is_empty() {
            return (None, messages);
        }
        (
            Some(Skill {
                name,
                description,
                tags: fm.metadata.tags,
                category: fm.metadata.category,
                path: dir.to_path_buf(),
                global: false,
                disable_model_invocation: fm.disable_model_invocation == Some(true),
                builtin: None,
            }),
            messages,
        )
    }

    /// Full prompt content for invoking this skill: the SKILL.md body without
    /// frontmatter, wrapped in a `<skill>` block with a relative-path hint,
    /// plus `args` after a blank line.
    pub fn invocation_content(&self, args: Option<&str>) -> std::io::Result<String> {
        // A built-in skill *is* its body: there is no directory to read, and so
        // no location to point a reference file at.
        let mut s = match self.builtin {
            Some(body) => format!(
                "<skill name=\"{}\" location=\"built in\">\n\n{body}\n</skill>",
                self.name
            ),
            None => {
                let skill_md = self.path.join("SKILL.md");
                let content = std::fs::read_to_string(&skill_md)?;
                format!(
                    "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
                    self.name,
                    skill_md.display(),
                    self.path.display(),
                    skill_body(&content)
                )
            }
        };
        if let Some(args) = args {
            s.push_str("\n\n");
            s.push_str(args);
        }
        Ok(s)
    }
}

pub(crate) fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name.chars().count() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.chars().count()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(
            "name contains invalid characters (lowercase a-z, 0-9, hyphens only)".to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

fn validate_description(description: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if description.trim().is_empty() {
        errors.push("description is required".to_string());
    } else if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        errors.push(format!(
            "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            description.chars().count()
        ));
    }
    errors
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\"' => out.push_str("&quot;"),
            '\u{27}' => out.push_str("&apos;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FrontMatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    metadata: FrontMatterMetadata,
    disable_model_invocation: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct FrontMatterMetadata {
    category: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// Split `---\n<yaml>\n---\n<body>` into the YAML and the trimmed body.
fn front_matter_split(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    Some((&rest[..end], rest[end + 4..].trim()))
}

/// The SKILL.md body without frontmatter; the whole trimmed content when no
/// frontmatter is present.
fn skill_body(content: &str) -> &str {
    match front_matter_split(content) {
        Some((_, body)) => body,
        None => content.trim(),
    }
}

fn parse_front_matter(content: &str) -> Option<FrontMatter> {
    let (yaml, _) = front_matter_split(content)?;
    yaml_serde::from_str(yaml).ok()
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
        let skills = Skills::load_from(&workspace, None, None, &config);
        let names: Vec<&str> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["ratatui", "tokio"]);
        assert_eq!(skills.skills[0].tags, vec!["rust"]);
        assert_eq!(skills.skills[1].category.as_deref(), Some("Test"));
        assert!(skills.warnings.is_empty());
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
        let skills = Skills::load_from(&workspace, None, None, &config);
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].description, "workspace copy");
        assert_eq!(skills.warnings.len(), 1);
        assert!(skills.warnings[0].message.contains("duplicate"));
    }

    #[test]
    fn discovers_nested_skills() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let nested = workspace.join(".agents/skills/ui/ratatui");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("SKILL.md"),
            "---\nname: ratatui\ndescription: TUI skill\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].name, "ratatui");
        assert_eq!(skills.skills[0].path, nested);
    }

    #[test]
    fn skips_hidden_and_node_modules_dirs() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        for path in [
            ".agents/skills/.hidden/sk",
            ".agents/skills/node_modules/sk",
        ] {
            let skill = workspace.join(path);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(
                skill.join("SKILL.md"),
                "---\nname: sk\ndescription: hidden skill\n---\n\nbody\n",
            )
            .unwrap();
        }

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert!(skills.skills.is_empty());
    }

    #[test]
    fn skill_root_stops_recursion() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let root = workspace.join(".agents/skills/ratatui");
        std::fs::create_dir_all(root.join("nested/inner")).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            "---\nname: ratatui\ndescription: TUI skill\n---\n\nbody\n",
        )
        .unwrap();
        std::fs::write(
            root.join("nested/inner/SKILL.md"),
            "---\nname: inner\ndescription: should not load\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].name, "ratatui");
    }

    #[test]
    fn missing_description_skips_with_warning() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/ratatui");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: ratatui\n---\n\nbody\n").unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert!(skills.skills.is_empty());
        assert_eq!(skills.warnings.len(), 1);
        assert_eq!(skills.warnings[0].message, "description is required");
        assert_eq!(skills.warnings[0].path, skill.join("SKILL.md"));
    }

    #[test]
    fn invalid_name_warns_but_loads() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/ratatui");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: Ratatui--X\ndescription: TUI skill\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.warnings.len(), 2);
        assert!(skills.warnings[0].message.contains("invalid characters"));
        assert!(skills.warnings[1].message.contains("consecutive hyphens"));
    }

    #[test]
    fn overlong_description_warns_but_loads() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/ratatui");
        std::fs::create_dir_all(&skill).unwrap();
        let description = "x".repeat(MAX_DESCRIPTION_LENGTH + 1);
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: ratatui\ndescription: {description}\n---\n\nbody\n"),
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.warnings.len(), 1);
        assert!(
            skills.warnings[0]
                .message
                .contains("exceeds 1024 characters")
        );
    }

    #[test]
    fn unparsable_frontmatter_skips_with_warning() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/ratatui");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "no frontmatter\n").unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert!(skills.skills.is_empty());
        assert_eq!(skills.warnings.len(), 1);
        assert!(skills.warnings[0].message.contains("frontmatter"));
    }

    #[test]
    fn duplicate_name_across_nesting_warns() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_skill(
            &workspace.join(".agents/skills"),
            "ratatui",
            "TUI skill",
            &[],
        );
        write_skill(
            &workspace.join(".agents/skills/group"),
            "ratatui",
            "group copy",
            &[],
        );

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.warnings.len(), 1);
        assert!(skills.warnings[0].message.contains("duplicate"));
        assert_eq!(
            skills.warnings[0].path,
            workspace.join(".agents/skills/ratatui/SKILL.md")
        );
    }

    #[test]
    fn location_dir_can_be_a_skill_root() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = dir.path().join("somewhere/ratatui");
        write_skill(Path::new(&skill), "ratatui", "TUI skill", &[]);
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();

        let config = SkillsConfig {
            disabled: false,
            dirs: vec![skill.to_string_lossy().into_owned()],
        };
        let skills = Skills::load_from(&workspace, None, None, &config);
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].name, "ratatui");
    }

    #[test]
    fn disabled_config_loads_nothing() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_skill(
            &workspace.join(".agents/skills"),
            "ratatui",
            "TUI skill",
            &[],
        );

        let config = SkillsConfig {
            disabled: true,
            dirs: Vec::new(),
        };
        let skills = Skills::load(&workspace, &config, &TrustGrants::all());
        assert!(skills.skills.is_empty());
        assert!(skills.warnings.is_empty());
    }

    #[test]
    fn untrusted_skills_skip_the_workspace_dir() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_skill(
            &workspace.join(".agents/skills"),
            "workspace-skill",
            "Workspace skill",
            &[],
        );
        let home = dir.path().join("home");
        write_skill(
            &home.join(".agents/skills"),
            "global-skill",
            "Global skill",
            &[],
        );

        let skills = Skills::load_from(&workspace, Some(&home), None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 2);

        let skills = Skills::load(&workspace, &SkillsConfig::default(), &TrustGrants::all());
        let names: HashSet<&str> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains("workspace-skill"));

        let skills = Skills::load(&workspace, &SkillsConfig::default(), &TrustGrants::none());
        let names: HashSet<&str> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains("workspace-skill"));
        assert!(skills.warnings.is_empty());
    }

    #[test]
    fn loads_home_agents_skills() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        write_skill(&home.join(".agents/skills"), "tokio", "Async skill", &[]);
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();

        let skills = Skills::load_from(&workspace, Some(&home), None, &SkillsConfig::default());
        let names: Vec<&str> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["tokio"]);
    }

    #[test]
    fn home_agents_skills_loses_to_workspace() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        write_skill(
            &workspace.join(".agents/skills"),
            "tokio",
            "workspace copy",
            &[],
        );
        write_skill(&home.join(".agents/skills"), "tokio", "home copy", &[]);

        let skills = Skills::load_from(&workspace, Some(&home), None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert_eq!(skills.skills[0].description, "workspace copy");
        assert_eq!(skills.warnings.len(), 1);
    }

    #[test]
    fn global_skill_dirs_precedence() {
        let home = dirs::home_dir().expect("a home dir for this platform");
        let global = shuvarie_config::config_dir().expect("a config dir for this platform");
        assert_eq!(
            global_skill_dirs(Some(&home), Some(&global)),
            vec![global.join("skills"), home.join(".agents").join("skills"),]
        );
        assert!(global_skill_dirs(None, None).is_empty());
    }

    #[test]
    fn invocation_content_wraps_body() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill_dir = workspace.join(".agents/skills/ratatui");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: ratatui\ndescription: TUI skill\n---\n\n# Ratatui\n\nbody text\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        let skill = &skills.skills[0];
        let content = skill.invocation_content(Some("focus on buffers")).unwrap();
        assert!(content.starts_with("<skill name=\"ratatui\""));
        assert!(content.contains(&format!(
            "location=\"{}\"",
            skill_dir.join("SKILL.md").display()
        )));
        assert!(content.contains("References are relative to"));
        assert!(content.contains("# Ratatui\n\nbody text"));
        assert!(!content.contains("description: TUI skill"));
        assert!(content.ends_with("</skill>\n\nfocus on buffers"));

        let plain = skill.invocation_content(None).unwrap();
        assert!(plain.ends_with("</skill>"));
    }

    #[test]
    fn invocation_content_without_frontmatter() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("raw");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Raw\n\nno frontmatter\n").unwrap();
        let skill = Skill {
            name: "raw".into(),
            description: "d".into(),
            tags: vec![],
            category: None,
            path: skill_dir,
            global: false,
            disable_model_invocation: false,
            builtin: None,
        };
        let content = skill.invocation_content(None).unwrap();
        assert!(content.contains("# Raw\n\nno frontmatter"));
    }

    #[test]
    fn invocation_content_with_crlf() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("crlf");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\r\nname: crlf\r\ndescription: windows line endings\r\n---\r\n\r\nbody\r\n",
        )
        .unwrap();
        let skill = Skill {
            name: "crlf".into(),
            description: "d".into(),
            tags: vec![],
            category: None,
            path: skill_dir,
            global: false,
            disable_model_invocation: false,
            builtin: None,
        };
        let content = skill.invocation_content(None).unwrap();
        assert!(content.contains("\nbody"));
        assert!(!content.contains("description:"));
    }

    #[test]
    fn the_codemode_reference_ships_only_when_codemode_is_on() {
        assert!(
            Skills::default().with_builtins(false).skills.is_empty(),
            "the reference teaches a language for a tool that is not offered"
        );

        let skills = Skills::default().with_builtins(true);
        assert_eq!(skills.skills.len(), 1);
        let skill = &skills.skills[0];
        assert_eq!(skill.name, shuvarie_codemode::skill::NAME);
        assert!(skill.builtin.is_some());
        assert!(
            !skill.disable_model_invocation,
            "the model loads it on demand"
        );
        // Built in, so there is no directory for a reference file to live in.
        assert_eq!(skill.path, PathBuf::new());
    }

    /// A user's own `codemode` skill is the workspace's answer to the same
    /// question, and it wins: replacing the reference must not need a fork.
    #[test]
    fn a_users_own_codemode_skill_wins() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_skill(&workspace.join(".agents/skills"), "codemode", "Mine.", &[]);

        let skills =
            Skills::load_from(&workspace, None, None, &SkillsConfig::default()).with_builtins(true);
        assert_eq!(skills.skills.len(), 1, "one skill, not two");
        assert!(
            skills.skills[0].builtin.is_none(),
            "the workspace's skill should have been kept"
        );
    }

    #[test]
    fn a_builtin_skill_is_invoked_from_its_body() {
        let skills = Skills::default().with_builtins(true);
        let content = skills.skills[0].invocation_content(None).unwrap();

        assert!(
            content.starts_with("<skill name=\"codemode\" location=\"built in\">"),
            "{content}"
        );
        assert!(content.contains("# Writing codemode scripts"), "{content}");
        // The file-skill preamble would be a lie: there is no directory.
        assert!(!content.contains("References are relative to"), "{content}");
        // Arguments still append, so `/skill:codemode ...` works.
        let with_args = skills.skills[0]
            .invocation_content(Some("why does trim fail?"))
            .unwrap();
        assert!(with_args.ends_with("why does trim fail?"), "{with_args}");
    }

    #[test]
    fn disable_model_invocation_hides_from_preamble() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skills_root = workspace.join(".agents/skills");
        write_skill(&skills_root, "ratatui", "TUI skill", &[]);
        let hidden = skills_root.join("secret");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(
            hidden.join("SKILL.md"),
            "---\nname: secret\ndescription: hidden skill\ndisable-model-invocation: true\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 2);
        let hidden = skills.skills.iter().find(|s| s.name == "secret").unwrap();
        assert!(hidden.disable_model_invocation);
        let normal = skills.skills.iter().find(|s| s.name == "ratatui").unwrap();
        assert!(!normal.disable_model_invocation);

        let section = skills.preamble_section().unwrap();
        assert!(section.contains("<name>ratatui</name>"));
        assert!(!section.contains("secret"));
    }

    #[test]
    fn preamble_none_when_all_hidden() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/secret");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: secret\ndescription: hidden skill\ndisable-model-invocation: true\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert!(skills.preamble_section().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_dir_loads_once() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let global = dir.path().join("global/skills");
        write_skill(&global, "ratatui", "TUI skill", &[]);
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();
        std::os::unix::fs::symlink(
            global.join("ratatui"),
            workspace.join(".agents/skills/ratatui"),
        )
        .unwrap();

        let config = SkillsConfig {
            disabled: false,
            dirs: vec![global.to_string_lossy().into_owned()],
        };
        let skills = Skills::load_from(&workspace, None, None, &config);
        assert_eq!(skills.skills.len(), 1);
        assert!(skills.warnings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn same_skill_md_via_two_dirs_loads_once() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let real = workspace.join(".agents/skills/ratatui");
        write_skill(real.parent().unwrap(), "ratatui", "TUI skill", &[]);
        let alias_dir = workspace.join(".agents/skills/alias");
        std::fs::create_dir_all(&alias_dir).unwrap();
        std::os::unix::fs::symlink(real.join("SKILL.md"), alias_dir.join("SKILL.md")).unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        assert_eq!(skills.skills.len(), 1);
        assert!(skills.warnings.is_empty());
    }

    #[test]
    fn home_skills_are_flagged_global() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        write_skill(
            &workspace.join(".agents/skills"),
            "ratatui",
            "TUI skill",
            &[],
        );
        write_skill(&home.join(".agents/skills"), "tokio", "Async skill", &[]);

        let skills = Skills::load_from(&workspace, Some(&home), None, &SkillsConfig::default());
        let ratatui = skills.skills.iter().find(|s| s.name == "ratatui").unwrap();
        assert!(!ratatui.global);
        let tokio = skills.skills.iter().find(|s| s.name == "tokio").unwrap();
        assert!(tokio.global);
    }

    #[test]
    fn config_dirs_outside_workspace_are_global() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let extra = dir.path().join("extra/skills");
        write_skill(&extra, "tokio", "Async skill", &[]);
        std::fs::create_dir_all(workspace.join(".agents/skills")).unwrap();

        let config = SkillsConfig {
            disabled: false,
            dirs: vec![extra.to_string_lossy().into_owned()],
        };
        let skills = Skills::load_from(&workspace, None, None, &config);
        let tokio = skills.skills.iter().find(|s| s.name == "tokio").unwrap();
        assert!(tokio.global);
        let workspace_only = workspace.join(".agents/skills/local");
        std::fs::create_dir_all(&workspace_only).unwrap();
        std::fs::write(
            workspace_only.join("SKILL.md"),
            "---\nname: local\ndescription: local skill\n---\n\nbody\n",
        )
        .unwrap();
        let skills = Skills::load_from(&workspace, None, None, &config);
        let local = skills.skills.iter().find(|s| s.name == "local").unwrap();
        assert!(!local.global);
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
        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        let section = skills.preamble_section().unwrap();
        assert!(section.contains("`skill` tool"));
        assert!(section.contains("<available_skills>"));
        assert!(section.contains("</available_skills>"));
        assert!(section.contains("<skill>"));
        assert!(section.contains("<name>ratatui</name>"));
        assert!(section.contains("<description>TUI skill</description>"));
        assert!(section.contains("<location>"));
        assert!(section.contains("SKILL.md"));
    }

    /// A built-in skill is reachable the same way, and the preamble has to say
    /// so rather than pointing at a directory that does not exist.
    #[test]
    fn the_preamble_lists_a_builtin_skill_as_built_in() {
        let section = Skills::default()
            .with_builtins(true)
            .preamble_section()
            .unwrap();
        assert!(section.contains("<name>codemode</name>"), "{section}");
        assert!(
            section.contains("<location>built in</location>"),
            "{section}"
        );
        assert!(section.contains("Writing Rhai scripts"), "{section}");
    }

    #[test]
    fn preamble_escapes_xml_specials() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/escape-test");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: escape-test\ndescription: uses <tags> & \"quotes\" 'and'\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        let section = skills.preamble_section().unwrap();
        assert!(section.contains("uses &lt;tags&gt; &amp; &quot;quotes&quot; &apos;and&apos;"));
        assert!(!section.contains("<tags>"));
        assert!(!section.contains("'and'"));
    }

    #[test]
    fn preamble_multiline_description_keeps_newlines() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let skill = workspace.join(".agents/skills/multiline");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: multiline\ndescription: |\n  first line\n  second line\n---\n\nbody\n",
        )
        .unwrap();

        let skills = Skills::load_from(&workspace, None, None, &SkillsConfig::default());
        let section = skills.preamble_section().unwrap();
        assert!(section.contains("first line\nsecond line"));
    }

    #[test]
    fn empty_when_no_skills() {
        let dir = TempDir::new().unwrap();
        let skills = Skills::load_from(dir.path(), None, None, &SkillsConfig::default());
        assert!(skills.is_empty());
        assert!(skills.preamble_section().is_none());
    }
}
