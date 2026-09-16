use std::sync::Arc;

use shuvarie_llm::ProviderClient;
use shuvarie_llm::TokenUsage;

use crate::Skills;
use crate::WebSearchConfig;
use crate::lsp_manager::SharedManager;
use crate::scenes::{Scene, ToolScene};
use crate::shell::Shell;
use crate::tools::{self, FileLocks, ShellOutputTx};

pub struct WorkerSet {
    pub workers: Vec<shuvarie_llm::WorkerAgent>,
    pub usage: Arc<std::sync::Mutex<TokenUsage>>,
}

/// One built-in worker: its name (the scene's `subagents` key), model-facing
/// description, and built-in preamble.
struct WorkerSpec {
    name: &'static str,
    description: &'static str,
    preamble: &'static str,
}

const WORKERS: [WorkerSpec; 3] = [
    WorkerSpec {
        name: "explore_workspace",
        description: "Explore the workspace: list directories, read files, and grep for text to locate, understand, and summarize code. Returns a concise report with file paths and line numbers. Use when a task requires understanding existing code before changes.",
        preamble: EXPLORER_PREAMBLE,
    },
    WorkerSpec {
        name: "run_tests",
        description: "Run the project's build, test, and lint commands, iterate on failures, and report the outcome. Returns a summary of what was run and the final status. Use to verify changes or diagnose failing commands.",
        preamble: TESTER_PREAMBLE,
    },
    WorkerSpec {
        name: "edit_files",
        description: "Read, write, edit, and delete files to implement changes in the workspace. Returns a summary of the files changed and what was done. Use when the task requires modifying code or other files.",
        preamble: EDITOR_PREAMBLE,
    },
];

/// Builds the worker roster under the scene: `subagents { disabled #true }`
/// empties the roster, a worker override replaces its built-in preamble (an
/// interlude appends to it — worker runs are single-shot with fresh history)
/// and gates its tool set, and a disabled worker disappears from the roster
/// (its tool vanishes from the main agent).
#[allow(clippy::too_many_arguments)]
pub fn build_workers(
    client: ProviderClient,
    model: &str,
    lsp: SharedManager,
    locks: FileLocks,
    worker_max_turns: usize,
    max_output_chars: usize,
    max_output_bytes: usize,
    context_budget: Option<shuvarie_llm::ContextBudget>,
    shell_tx: ShellOutputTx,
    shell: Shell,
    access: crate::permissions::Access,
    scene: &Scene,
    web_search: Option<&WebSearchConfig>,
    skills: &Skills,
) -> WorkerSet {
    let usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let roster_disabled = scene.subagents().map(|s| s.disabled).unwrap_or(false);
    let mut workers = Vec::new();
    if !roster_disabled {
        for (spec, tool_set) in [
            (&WORKERS[0], WorkerTools::Read),
            (&WORKERS[1], WorkerTools::Command),
            (&WORKERS[2], WorkerTools::Edit),
        ] {
            let config = scene.worker(spec.name);
            if config.is_some_and(|w| w.disabled) {
                continue;
            }
            let mut preamble = config
                .and_then(|w| w.system_prompts.prelude.as_deref())
                .unwrap_or(spec.preamble)
                .to_string();
            if let Some(interlude) = config.and_then(|w| w.system_prompts.interlude.as_deref()) {
                preamble.push_str("\n\n");
                preamble.push_str(interlude);
            }
            let tool_scene = ToolScene::build(config.map(|w| &w.tools));
            let worker_tools = match tool_set {
                WorkerTools::Read => tools::read_tools(
                    lsp.clone(),
                    tools::ReadCache::new(),
                    max_output_chars,
                    max_output_bytes,
                    access.clone(),
                    &tool_scene,
                    web_search,
                    skills,
                ),
                WorkerTools::Command => tools::command_tools(
                    shell_tx.tagged(spec.name),
                    shell.clone(),
                    access.clone(),
                    &tool_scene,
                ),
                WorkerTools::Edit => tools::edit_tools(
                    lsp.clone(),
                    locks.clone(),
                    tools::ReadCache::new(),
                    max_output_chars,
                    max_output_bytes,
                    access.clone(),
                    &tool_scene,
                ),
            };
            workers.push(shuvarie_llm::WorkerAgent::new(
                spec.name,
                spec.description,
                &preamble,
                client.clone(),
                model,
                worker_tools,
                Arc::clone(&usage),
                worker_max_turns,
                context_budget.clone(),
            ));
        }
    }
    WorkerSet { workers, usage }
}

enum WorkerTools {
    Read,
    Command,
    Edit,
}

const EXPLORER_PREAMBLE: &str = "\
You are the workspace explorer worker for Shuvarie, an agentic coding assistant. \
You search and read the user's project to understand it. Prefer using your tools to \
inspect the workspace instead of guessing. Finish with a concise report of what you \
found, including file paths and line numbers where relevant.";

const TESTER_PREAMBLE: &str = "\
You are the command runner worker for Shuvarie, an agentic coding assistant. \
You run the project's build, test, and lint commands and iterate on failures until \
they pass or you are confident the cause is external. When a tool reports an error, \
fix the cause and retry rather than stopping. Finish with a short summary of what \
you ran and the final result.";

const EDITOR_PREAMBLE: &str = "\
You are the editor worker for Shuvarie, an agentic coding assistant. \
You read, write, and edit files to implement the requested changes. Prefer reading \
the target files first to understand the existing code before editing. For more complex \
editing — changes spanning multiple files, renames/moves, or adding files — \
use the `apply_patch` tool: it applies one `*** Begin Patch` … `*** End Patch` envelope \
touching several files in a single call. Delete individual files with the \
`delete_file` tool. After \
finishing, summarize the files you changed and what you did in a short reply.";

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use shuvarie_config::SceneConfig;
    use shuvarie_config::ScenesConfig;

    fn scene(name: &str, config: SceneConfig) -> Scene {
        let mut scenes = ScenesConfig::default();
        scenes.scenes.insert(name.to_string(), config);
        Scene::resolve(&scenes, Some(name))
    }

    fn build(scene: &Scene) -> WorkerSet {
        let (tx, _rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(4);
        build_workers(
            ProviderClient::build(selune::ProviderType::Ollama, None, None).unwrap(),
            "test-model",
            std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_lsp::LspManager::new(
                std::path::PathBuf::from("."),
                false,
                Default::default(),
            ))),
            FileLocks::new(),
            1,
            100,
            100,
            None,
            ShellOutputTx::new(tx),
            crate::shell::resolve(None).shell,
            crate::test_util::access(),
            scene,
            None,
            &Skills::default(),
        )
    }

    fn names(set: &WorkerSet) -> Vec<&str> {
        set.workers.iter().map(|w| w.name()).collect()
    }

    #[test]
    fn default_scene_builds_the_full_roster() {
        let set = build(&Scene::default());
        assert_eq!(
            names(&set),
            vec!["explore_workspace", "run_tests", "edit_files"]
        );
    }

    #[test]
    fn subagents_disabled_empties_the_roster() {
        let scene = scene(
            "Solo",
            SceneConfig {
                subagents: shuvarie_config::SubagentsConfig {
                    disabled: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(build(&scene).workers.is_empty());
    }

    #[test]
    fn worker_override_replaces_the_preamble_and_disables_the_worker() {
        let scene = scene(
            "Plan",
            SceneConfig {
                subagents: shuvarie_config::SubagentsConfig {
                    disabled: false,
                    workers: BTreeMap::from([
                        (
                            "edit_files".to_string(),
                            shuvarie_config::SubagentConfig {
                                disabled: true,
                                ..Default::default()
                            },
                        ),
                        (
                            "explore_workspace".to_string(),
                            shuvarie_config::SubagentConfig {
                                system_prompts: shuvarie_config::SystemPromptsConfig {
                                    prelude: Some("You are a librarian.".into()),
                                    interlude: Some("Stay quiet.".into()),
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                        ),
                    ]),
                },
                ..Default::default()
            },
        );
        let set = build(&scene);
        assert_eq!(names(&set), vec!["explore_workspace", "run_tests"]);
        let explorer = &set.workers[0];
        assert_eq!(
            explorer.preamble(),
            "You are a librarian.\n\nStay quiet.",
            "the override replaces the preamble and the interlude appends"
        );
    }
}
