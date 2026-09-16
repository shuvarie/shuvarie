use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::Skills;

use super::arg_value;

pub(crate) struct SkillTool {
    skills: Skills,
    max_output_chars: usize,
}

impl SkillTool {
    pub(crate) fn new(skills: Skills, max_output_chars: usize) -> Self {
        Self {
            skills,
            max_output_chars,
        }
    }
}

impl Tool for SkillTool {
    const NAME: &'static str = "skill";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Load one of the available skills by name and return its SKILL.md instructions. \
         Use it when a task matches a listed skill's description, before starting work: \
         the returned instructions tell you how to approach the task. A skill's SKILL.md \
         may reference further resource files (scripts, docs, data) relative to its \
         directory; load one with the `path` argument to follow the reference."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill": { "type": "string", "description": "Name of the skill to load (see the available_skills preamble block)" },
                "path": { "type": "string", "description": "Resource file to read instead of SKILL.md, relative to the skill's directory" }
            },
            "required": ["skill"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let skills = self.skills.clone();
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let name = arg_value(&args, "skill")?;
            let skill = skills
                .skills
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(&name))
                .ok_or_else(|| unknown_skill(&skills, &name))?;
            if skill.disable_model_invocation {
                return Err(format!(
                    "skill '{name}' is user-invocation only; ask the user to run /skill:{name}"
                ));
            }
            let text = match args.get("path").and_then(Value::as_str) {
                Some(rel) => read_resource(skill, rel).await?,
                None => skill
                    .invocation_content(None)
                    .map_err(|e| format!("read SKILL.md of '{name}': {e}"))?,
            };
            let hint = format!("loaded by the `skill` tool ({name})");
            if let Some(capped) = crate::truncate::truncate_output(&text, max_output_chars, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            Ok(ToolOutput::text(text))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

async fn read_resource(skill: &crate::Skill, rel: &str) -> Result<String, String> {
    let rel_path = std::path::Path::new(rel);
    if rel.trim().is_empty() {
        return Err("empty 'path' argument".into());
    }
    if rel_path.is_absolute() {
        return Err(format!(
            "'path' must be relative to the skill directory: {rel}"
        ));
    }
    let dir =
        std::fs::canonicalize(&skill.path).map_err(|e| format!("{}: {e}", skill.path.display()))?;
    let canonical = dir
        .join(rel_path)
        .canonicalize()
        .map_err(|e| format!("{rel}: {e}"))?;
    if !canonical.starts_with(&dir) {
        return Err(format!("{rel} resolves outside the skill directory"));
    }
    if canonical.is_dir() {
        return Err(format!("'{rel}' is a directory, not a file"));
    }
    let data = tokio::fs::read(&canonical)
        .await
        .map_err(|e| format!("read {rel}: {e}"))?;
    if data.contains(&0) {
        return Err(format!(
            "'{rel}' is binary ({} bytes); run it with run_shell instead of reading it",
            data.len()
        ));
    }
    Ok(format!(
        "{rel} (at {}):\n\n{}",
        canonical.display(),
        String::from_utf8_lossy(&data)
    ))
}

fn unknown_skill(skills: &Skills, name: &str) -> String {
    let mut names: Vec<&str> = skills.skills.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    if names.is_empty() {
        format!("unknown skill '{name}': no skills are available")
    } else {
        format!(
            "unknown skill '{name}': available skills are {}",
            names.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};
    use crate::{Skill, SkillWarning};
    use std::path::PathBuf;

    fn skill_fixture() -> Skills {
        Skills {
            skills: vec![Skill {
                name: "demo".to_string(),
                description: "demo skill".to_string(),
                tags: vec![],
                category: None,
                path: PathBuf::from(".agents/skills/demo"),
                global: false,
                disable_model_invocation: false,
            }],
            warnings: Vec::<SkillWarning>::new(),
        }
    }

    #[tokio::test]
    async fn skill_loads_skill_md() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".agents/skills/demo").unwrap();
        std::fs::write(
            ".agents/skills/demo/SKILL.md",
            "# Demo\nUse the demo steps.\n",
        )
        .unwrap();
        let tool = SkillTool::new(skill_fixture(), 0);
        let out = tool
            .call(&mut new_ctx(), json!({ "skill": "demo" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("<skill name=\"demo\""), "{text}");
        assert!(text.contains("Use the demo steps."), "{text}");
        assert!(text.contains("References are relative to"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn skill_name_match_is_case_insensitive() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".agents/skills/demo").unwrap();
        std::fs::write(".agents/skills/demo/SKILL.md", "# Demo\nbody\n").unwrap();
        let tool = SkillTool::new(skill_fixture(), 0);
        let out = tool
            .call(&mut new_ctx(), json!({ "skill": "DEMO" }))
            .await
            .unwrap();
        assert!(out.as_text().unwrap().contains("body"));
        drop(dir);
    }

    #[tokio::test]
    async fn skill_unknown_name_lists_available() {
        let (dir, _guard) = tempdir();
        let tool = SkillTool::new(skill_fixture(), 0);
        let err = tool
            .call(&mut new_ctx(), json!({ "skill": "missing" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("available skills are demo"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn skill_rejects_model_disabled_skills() {
        let (dir, _guard) = tempdir();
        let mut fixture = skill_fixture();
        fixture.skills[0].disable_model_invocation = true;
        let tool = SkillTool::new(fixture, 0);
        let err = tool
            .call(&mut new_ctx(), json!({ "skill": "demo" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("user-invocation only"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn skill_reads_resource_within_dir() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".agents/skills/demo/scripts").unwrap();
        std::fs::write(".agents/skills/demo/SKILL.md", "# Demo\n").unwrap();
        std::fs::write(".agents/skills/demo/scripts/run.py", "print('hi')\n").unwrap();
        let tool = SkillTool::new(skill_fixture(), 0);
        let out = tool
            .call(
                &mut new_ctx(),
                json!({ "skill": "demo", "path": "scripts/run.py" }),
            )
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("scripts/run.py (at "), "{text}");
        assert!(text.contains("print('hi')"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn skill_resource_cannot_escape_skill_dir() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".agents/skills/demo").unwrap();
        std::fs::write(".agents/skills/demo/SKILL.md", "# Demo\n").unwrap();
        std::fs::write("secret.txt", "nope\n").unwrap();
        let tool = SkillTool::new(skill_fixture(), 0);
        let err = tool
            .call(
                &mut new_ctx(),
                json!({ "skill": "demo", "path": "../../../secret.txt" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside the skill directory"),
            "{}",
            err.to_string()
        );
        let err = tool
            .call(
                &mut new_ctx(),
                json!({ "skill": "demo", "path": "/etc/hostname" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must be relative"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn skill_refuses_binary_resources() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".agents/skills/demo").unwrap();
        std::fs::write(".agents/skills/demo/SKILL.md", "# Demo\n").unwrap();
        std::fs::write(".agents/skills/demo/blob.bin", [0, 1, 2, 3]).unwrap();
        let tool = SkillTool::new(skill_fixture(), 0);
        let err = tool
            .call(
                &mut new_ctx(),
                json!({ "skill": "demo", "path": "blob.bin" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "{}", err.to_string());
        drop(dir);
    }

    #[tokio::test]
    async fn skill_truncates_oversized_output() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".agents/skills/demo").unwrap();
        std::fs::write(".agents/skills/demo/SKILL.md", "x".repeat(10_000) + "\n").unwrap();
        let tool = SkillTool::new(skill_fixture(), 100);
        let out = tool
            .call(&mut new_ctx(), json!({ "skill": "demo" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("truncated"), "{text}");
        assert!(text.chars().count() < 10_400, "{}", text.chars().count());
        drop(dir);
    }
}
