mod delete_file;
mod edit_file;
mod glob;
mod grep;
mod list_dir;
mod lsp;
mod question;
mod read_file;
mod run_shell;
mod skill;
pub mod todos;
mod web_search;
mod webfetch;
mod write_file;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use shuvarie_llm::{DiffLine, DiffLineKind, DynamicTool, Tool, ToolExecutionError, ToolOutput};

use crate::Skills;
use crate::WebSearchConfig;
use crate::lsp_manager::SharedManager;
use crate::permissions::Access;
use crate::question::QuestionGate;
use crate::scenes::ToolScene;
use crate::shell::Shell;

use edit_file::EditFile;
use glob::Glob;
use grep::Grep;
use list_dir::ListDir;
use lsp::Lsp;
use question::Question;
use read_file::ReadFile;
use run_shell::RunShell;
use skill::SkillTool;
use web_search::WebSearch;
use webfetch::WebFetch;
use write_file::WriteFile;

use delete_file::DeleteFile;

pub(crate) use run_shell::run_shell_command;
pub use run_shell::{ShellChunk, ShellOutputTx};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

type ReadKey = (String, Option<u64>, Option<u64>);

/// Builds one roster tool under the scene: disabled tools are skipped, and a
/// scene `ask` on a tool that does not authorize through the permission
/// engine wraps the call in a confirmation prompt (the gated tools get the
/// same overlay through `Access::for_tool` instead). `gated` marks tools
/// whose calls authorize via `Access`.
fn scene_tool<T>(
    name: &'static str,
    scene: &ToolScene,
    access: &Access,
    gated: bool,
    build: impl FnOnce(Access) -> T,
) -> Option<DynamicTool>
where
    T: Tool<Args = serde_json::Value, Output = ToolOutput, Error = ToolExecutionError> + 'static,
{
    if !scene.allows(name) {
        return None;
    }
    let access = access.for_tool(name, scene);
    let ask_reason = (!gated)
        .then(|| access.scene_ask_reason().map(str::to_string))
        .flatten();
    let tool = build(access.clone());
    if let Some(reason) = ask_reason {
        let tool = Arc::new(tool);
        Some(DynamicTool::new(
            name,
            tool.description(),
            tool.parameters(),
            move |ctx, args| {
                let tool = Arc::clone(&tool);
                let access = access.clone();
                let reason = reason.to_string();
                Box::pin(async move {
                    if let Err(err) = access.confirm_scene(&reason).await {
                        return Err(ToolExecutionError::other(err));
                    }
                    tool.call(ctx, args).await
                })
            },
        ))
    } else {
        Some(shuvarie_llm::into_dynamic(name, tool))
    }
}

/// Per-turn dedupe cache for `read_file`: tracks `(path, offset, limit)` keys
/// that have already been returned to the model this turn, plus a set of all
/// paths read this turn (any range) used by `write_file`'s read-first check
/// for overwrites. Repeated identical reads get a short note instead of
/// re-sending file content, which keeps the agent loop from blowing up the
/// context by re-reading the same large file.
#[derive(Clone, Default)]
pub struct ReadCache {
    seen: Arc<Mutex<std::collections::HashSet<ReadKey>>>,
    read_paths: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl ReadCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if this exact `(path, offset, limit)` was already read
    /// this turn, otherwise records it and returns `false`.
    fn mark(&self, path: &str, offset: Option<u64>, limit: Option<u64>) -> bool {
        let key = (path.to_string(), offset, limit);
        let mut seen = self.seen.lock().unwrap();
        let fresh = seen.insert(key);
        if fresh {
            self.read_paths.lock().unwrap().insert(path.to_string());
        }
        !fresh
    }

    /// Returns `true` if the path was read (any range) earlier this turn.
    pub(crate) fn was_read(&self, path: &str) -> bool {
        self.read_paths.lock().unwrap().contains(path)
    }
}

/// Per-target-file mutation locks shared by `write_file`, `edit_file`, and
/// `apply_patch`, so worker agents and the manager never interleave
/// read-modify-write spans on the same file. Different files stay parallel.
#[derive(Clone, Default)]
pub struct FileLocks {
    locks: Arc<tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl FileLocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn lock(&self, path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = self
            .locks
            .lock()
            .await
            .entry(path.to_owned())
            .or_default()
            .clone();
        entry.lock_owned().await
    }
}

pub(crate) fn arg_value(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn workspace_root() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cwd: {e}"))
}

pub(crate) fn compute_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut options = similar::InlineChangeOptions::new();
    options.mode(similar::InlineChangeMode::UnicodeWords);
    let mut lines: Vec<DiffLine> = Vec::new();
    for group in diff.grouped_ops(3) {
        if !lines.is_empty() {
            lines.push(DiffLine {
                kind: DiffLineKind::Ellipsis,
                old_line: None,
                new_line: None,
                text: String::new(),
                edits: Vec::new(),
            });
        }
        for op in group {
            for change in diff.iter_inline_changes_with_options(&op, options) {
                let (kind, old_line, new_line) = match change.tag() {
                    similar::ChangeTag::Delete => (DiffLineKind::Remove, change.old_index(), None),
                    similar::ChangeTag::Insert => (DiffLineKind::Add, None, change.new_index()),
                    similar::ChangeTag::Equal => (
                        DiffLineKind::Context,
                        change.old_index(),
                        change.new_index(),
                    ),
                };
                let old_line = old_line.map(|i| i as u64 + 1);
                let new_line = new_line.map(|i| i as u64 + 1);
                let mut text = String::new();
                let mut edits = Vec::new();
                for &(emphasized, value) in change.values() {
                    let start = text.len() as u32;
                    text.push_str(value);
                    if emphasized {
                        edits.push((start, text.len() as u32));
                    }
                }
                let trimmed = text.trim_end_matches(['\r', '\n']).len() as u32;
                edits.retain_mut(|(start, end)| {
                    *end = (*end).min(trimmed);
                    *start < *end
                });
                lines.push(DiffLine {
                    kind,
                    old_line,
                    new_line,
                    text,
                    edits,
                });
            }
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
pub fn all_tools(
    lsp: SharedManager,
    locks: FileLocks,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    question_gate: QuestionGate,
    access: Access,
    shell_tx: ShellOutputTx,
    shell: Shell,
    todo_state: todos::TodoState,
    scene: &ToolScene,
    web_search: Option<&WebSearchConfig>,
    skills: &Skills,
) -> Vec<DynamicTool> {
    let mut tools = Vec::new();
    tools.extend(scene_tool("read_file", scene, &access, true, |access| {
        ReadFile::new(
            read_cache.clone(),
            max_output_chars,
            max_output_bytes,
            access,
        )
    }));
    tools.extend(scene_tool("write_file", scene, &access, true, |access| {
        WriteFile::new(read_cache.clone(), Some(lsp.clone()), locks.clone(), access)
    }));
    tools.extend(scene_tool("edit_file", scene, &access, true, |access| {
        EditFile::new(Some(lsp.clone()), locks.clone(), access)
    }));
    tools.extend(scene_tool("apply_patch", scene, &access, true, |access| {
        crate::apply_patch::ApplyPatch::new(Some(lsp.clone()), locks.clone(), access)
    }));
    tools.extend(scene_tool("delete_file", scene, &access, true, |access| {
        DeleteFile::new(locks.clone(), access)
    }));
    tools.extend(scene_tool("run_shell", scene, &access, true, |access| {
        RunShell::new(shell_tx.clone(), shell.clone(), access)
    }));
    tools.extend(scene_tool("list_dir", scene, &access, true, |access| {
        ListDir::new(access)
    }));
    tools.extend(scene_tool("grep", scene, &access, true, |access| {
        Grep::new(access)
    }));
    tools.extend(scene_tool("glob", scene, &access, true, |access| {
        Glob::new(access)
    }));
    if !skills.is_empty() {
        tools.extend(scene_tool(
            SkillTool::NAME,
            scene,
            &access,
            false,
            |_access| SkillTool::new(skills.clone(), max_output_chars),
        ));
    }
    tools.extend(scene_tool("lsp", scene, &access, false, |_access| {
        Lsp::new(lsp.clone())
    }));
    tools.extend(scene_tool("webfetch", scene, &access, false, |_access| {
        WebFetch::new(max_output_chars)
    }));
    tools.extend(scene_tool("question", scene, &access, false, |_access| {
        Question::new(question_gate)
    }));
    tools.extend(scene_tool(
        todos::Todo::NAME,
        scene,
        &access,
        false,
        |_access| todos::Todo::new(todo_state),
    ));
    if let Some(config) = web_search {
        tools.extend(scene_tool(
            WebSearch::NAME,
            scene,
            &access,
            false,
            |_access| WebSearch::new(config, max_output_chars),
        ));
    }
    tools
}

#[allow(clippy::too_many_arguments)]
pub fn read_tools(
    lsp: SharedManager,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: Access,
    scene: &ToolScene,
    web_search: Option<&WebSearchConfig>,
    skills: &Skills,
) -> Vec<DynamicTool> {
    let mut tools = Vec::new();
    tools.extend(scene_tool("read_file", scene, &access, true, |access| {
        ReadFile::new(
            read_cache.clone(),
            max_output_chars,
            max_output_bytes,
            access,
        )
    }));
    tools.extend(scene_tool("list_dir", scene, &access, true, |access| {
        ListDir::new(access)
    }));
    tools.extend(scene_tool("grep", scene, &access, true, |access| {
        Grep::new(access)
    }));
    tools.extend(scene_tool("glob", scene, &access, true, |access| {
        Glob::new(access)
    }));
    if !skills.is_empty() {
        tools.extend(scene_tool(
            SkillTool::NAME,
            scene,
            &access,
            false,
            |_access| SkillTool::new(skills.clone(), max_output_chars),
        ));
    }
    tools.extend(scene_tool("lsp", scene, &access, false, |_access| {
        Lsp::new(lsp.clone())
    }));
    tools.extend(scene_tool("webfetch", scene, &access, false, |_access| {
        WebFetch::new(max_output_chars)
    }));
    if let Some(config) = web_search {
        tools.extend(scene_tool(
            WebSearch::NAME,
            scene,
            &access,
            false,
            |_access| WebSearch::new(config, max_output_chars),
        ));
    }
    tools
}

pub fn command_tools(
    shell_tx: ShellOutputTx,
    shell: Shell,
    access: Access,
    scene: &ToolScene,
) -> Vec<DynamicTool> {
    scene_tool("run_shell", scene, &access, true, |access| {
        RunShell::new(shell_tx, shell, access)
    })
    .into_iter()
    .collect()
}

pub fn edit_tools(
    lsp: SharedManager,
    locks: FileLocks,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: Access,
    scene: &ToolScene,
) -> Vec<DynamicTool> {
    let mut tools = Vec::new();
    tools.extend(scene_tool("read_file", scene, &access, true, |access| {
        ReadFile::new(
            read_cache.clone(),
            max_output_chars,
            max_output_bytes,
            access,
        )
    }));
    tools.extend(scene_tool("write_file", scene, &access, true, |access| {
        WriteFile::new(read_cache, Some(lsp.clone()), locks.clone(), access)
    }));
    tools.extend(scene_tool("edit_file", scene, &access, true, |access| {
        EditFile::new(Some(lsp.clone()), locks.clone(), access)
    }));
    tools.extend(scene_tool("apply_patch", scene, &access, true, |access| {
        crate::apply_patch::ApplyPatch::new(Some(lsp.clone()), locks.clone(), access)
    }));
    tools.extend(scene_tool("delete_file", scene, &access, true, |access| {
        DeleteFile::new(locks.clone(), access)
    }));
    tools.extend(scene_tool("lsp", scene, &access, false, |_access| {
        Lsp::new(lsp)
    }));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{resolve_read, resolve_write};
    use crate::test_util::tempdir;
    use shuvarie_config::{SceneToolVerb, SceneToolsConfig, ToolOverride};
    use tempfile::TempDir;

    /// The full roster built for one scene, as the core task does per turn.
    fn roster_with(skills: &Skills, scene: &ToolScene) -> Vec<String> {
        let (question_tx, _question_rx) = tokio::sync::mpsc::channel(1);
        let (shell_tx, _shell_rx) = tokio::sync::mpsc::channel(1);
        let lsp = std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_lsp::LspManager::new(
            std::path::PathBuf::from("."),
            false,
            Default::default(),
        )));
        all_tools(
            lsp,
            FileLocks::new(),
            ReadCache::new(),
            100,
            100,
            QuestionGate::new(question_tx),
            crate::test_util::access(),
            ShellOutputTx::new(shell_tx),
            crate::shell::resolve(None).shell,
            todos::TodoState::from_records(&[]),
            scene,
            None,
            skills,
        )
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect()
    }

    fn roster(scene: &ToolScene) -> Vec<String> {
        roster_with(&Skills::default(), scene)
    }

    #[test]
    fn the_default_scene_builds_every_tool() {
        let names = roster(&ToolScene::default());
        for expected in [
            "read_file",
            "write_file",
            "edit_file",
            "apply_patch",
            "delete_file",
            "run_shell",
            "list_dir",
            "grep",
            "glob",
            "lsp",
            "webfetch",
            "question",
            "todo",
        ] {
            assert!(
                names.contains(&expected.into()),
                "missing {expected}: {names:?}"
            );
        }
    }

    #[test]
    fn disable_all_leaves_only_reenabled_tools() {
        let mut tools = SceneToolsConfig {
            verb: Some(SceneToolVerb::DisableAll),
            ..SceneToolsConfig::default()
        };
        tools.tools.insert(
            "read_file".into(),
            ToolOverride {
                disabled: Some(false),
                ask: None,
            },
        );
        let names = roster(&ToolScene::build(Some(&tools)));
        assert_eq!(names, vec!["read_file".to_string()]);
    }

    #[test]
    fn disable_all_with_no_reenabled_tools_builds_an_empty_roster() {
        let tools = SceneToolsConfig {
            verb: Some(SceneToolVerb::DisableAll),
            ..SceneToolsConfig::default()
        };
        let names = roster(&ToolScene::build(Some(&tools)));
        assert!(names.is_empty(), "roster: {names:?}");
    }

    #[test]
    fn skill_tool_enters_the_roster_only_with_skills() {
        let mut skills = Skills::default();
        skills.skills.push(crate::Skill {
            name: "demo".into(),
            description: "d".into(),
            tags: vec![],
            category: None,
            path: std::path::PathBuf::from("."),
            global: false,
            disable_model_invocation: false,
        });
        let names = roster_with(&skills, &ToolScene::default());
        assert!(names.contains(&"skill".into()), "roster: {names:?}");
        assert!(!roster(&ToolScene::default()).contains(&"skill".into()));

        let mut tools = SceneToolsConfig {
            verb: Some(SceneToolVerb::DisableAll),
            ..SceneToolsConfig::default()
        };
        tools.tools.insert(
            "skill".into(),
            ToolOverride {
                disabled: Some(false),
                ask: None,
            },
        );
        let names = roster_with(&skills, &ToolScene::build(Some(&tools)));
        assert_eq!(names, vec!["skill".to_string()]);
    }

    #[test]
    fn diff_computes_line_numbers_and_ellipsis() {
        let old = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let new = "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let diff = compute_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Remove));
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Add));
        let remove = diff
            .iter()
            .find(|l| l.kind == DiffLineKind::Remove)
            .unwrap();
        assert_eq!(remove.old_line, Some(3));
        assert_eq!(remove.text, "three\n");
        let add = diff.iter().find(|l| l.kind == DiffLineKind::Add).unwrap();
        assert_eq!(add.new_line, Some(3));
        assert_eq!(add.text, "THREE\n");
    }

    #[test]
    fn diff_inserts_ellipsis_between_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let new = "A\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\n";
        let diff = compute_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Ellipsis));
    }

    #[test]
    fn diff_marks_partial_edits() {
        let lines = compute_diff("let value = compute(x);\n", "let value = compute(y);\n");
        assert_eq!(
            (lines[0].kind, lines[1].kind),
            (DiffLineKind::Remove, DiffLineKind::Add)
        );
        assert_eq!(
            lines[0].edits, lines[1].edits,
            "paired rows emphasize the same run"
        );
        let (start, end) = lines[0].edits[0];
        assert_eq!(&lines[0].text[start as usize..end as usize], "x");
        assert_eq!(&lines[1].text[start as usize..end as usize], "y");
        assert!(
            (end as usize) <= lines[0].text.trim_end_matches(['\r', '\n']).len(),
            "ranges must exclude the row break"
        );

        let lines = compute_diff("x\n", "x\ny\n");
        assert!(
            lines.iter().all(|line| line.edits.is_empty()),
            "unpaired rows stay unemphasized: {lines:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tilde_paths_expand() {
        let (dir, _guard) = tempdir();
        let original_home = std::env::var("HOME").ok();
        let home = TempDir::new().unwrap();
        unsafe { std::env::set_var("HOME", home.path()) };
        std::fs::write(home.path().join("homefile.txt"), "x").unwrap();
        let abs = resolve_write("~/homefile.txt").unwrap();
        assert_eq!(abs, home.path().join("homefile.txt"));
        match original_home {
            Some(home_path) => unsafe { std::env::set_var("HOME", home_path) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        drop(dir);
    }

    #[test]
    fn resolve_read_classification() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".git").unwrap();
        std::fs::write(".git/config", "x").unwrap();
        assert!(resolve_read(".git/config").is_ok());
        assert!(resolve_read(".").is_ok());
        assert!(resolve_write("src/f.txt").is_ok());

        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-outside-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(resolve_read(&rel).is_ok());
        assert!(resolve_write(&rel).is_ok());
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }
}
