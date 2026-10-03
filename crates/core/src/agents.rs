use std::sync::Arc;

use shuvarie_llm::ProviderClient;
use shuvarie_llm::TokenUsage;

use crate::Skills;
use crate::WebSearchConfig;
use crate::lsp_manager::SharedManager;
use crate::scenes::{Scene, ToolScene};
use crate::shell::Shell;
use crate::tools::{self, FileLocks, ShellOutputTx};
use shuvarie_config::SubagentToolset;

pub struct WorkerSet {
    pub workers: Vec<shuvarie_llm::WorkerAgent>,
    pub usage: Arc<std::sync::Mutex<TokenUsage>>,
}

/// One built-in worker: its name (the scene's `subagents` key), model-facing
/// description, built-in preamble, and default tool set.
struct WorkerSpec {
    name: &'static str,
    description: &'static str,
    preamble: &'static str,
    tool_set: SubagentToolset,
}

/// The fixed built-in workers, in roster order.
const WORKERS: [WorkerSpec; 3] = [
    WorkerSpec {
        name: "explore_workspace",
        description: "Explore the workspace: list directories, read files, and grep for text to locate, understand, and summarize code. Returns a concise report with file paths and line numbers. Use when a task requires understanding existing code before changes.",
        preamble: EXPLORER_PREAMBLE,
        tool_set: SubagentToolset::Read,
    },
    WorkerSpec {
        name: "run_tests",
        description: "Run the project's build, test, and lint commands, iterate on failures, and report the outcome. Returns a summary of what was run and the final status. Use to verify changes or diagnose failing commands.",
        preamble: TESTER_PREAMBLE,
        tool_set: SubagentToolset::Command,
    },
    WorkerSpec {
        name: "edit_files",
        description: "Read, write, edit, and delete files to implement changes in the workspace. Returns a summary of the files changed and what was done. Use when the task requires modifying code or other files.",
        preamble: EDITOR_PREAMBLE,
        tool_set: SubagentToolset::Edit,
    },
];

/// The extra-worker fallbacks: a description when the entry ships none, and
/// a preamble when it sets no prelude.
const EXTRA_WORKER_DESCRIPTION: &str =
    "A custom subagent worker: complete the given task and report concisely.";

const EXTRA_WORKER_PREAMBLE: &str = "You are a subagent worker for Shuvarie, an agentic coding assistant. \
You receive one task from the main agent: carry it out with your tools where they \
help, and finish with a concise report of what you did and found.";

/// Builds the worker roster under the scene: `subagents { disabled }`
/// empties the roster, a worker override replaces its built-in preamble (an
/// interlude appends to it — worker runs are single-shot with fresh history)
/// and gates its tool set (its `toolset` may also swap the built-in tool
/// set), and a disabled worker disappears from the roster (its tool vanishes
/// from the main agent). A `subagents` entry keyed by anything else defines
/// a new extra worker: `description` is the model-facing tool description
/// and `toolset` (default `read`) picks its tool set, still gated by the
/// entry's per-tool overrides.
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
    office_converter: Option<&str>,
    skills: &Skills,
) -> WorkerSet {
    let usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let roster_disabled = scene.subagents().map(|s| s.disabled).unwrap_or(false);
    let mut workers = Vec::new();
    if !roster_disabled {
        for spec in &WORKERS {
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
            let tool_set = config.and_then(|w| w.toolset).unwrap_or(spec.tool_set);
            let worker_tools = worker_tools(
                tool_set,
                lsp.clone(),
                locks.clone(),
                max_output_chars,
                max_output_bytes,
                access.clone(),
                &tool_scene,
                web_search,
                office_converter,
                skills,
                shell_tx.tagged(spec.name),
                shell.clone(),
            );
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
        // Extra workers: `subagents` entries keyed by anything but a built-in
        // worker name, in name order.
        if let Some(subagents) = scene.subagents() {
            for (name, config) in subagents
                .workers
                .iter()
                .filter(|(name, _)| !WORKERS.iter().any(|spec| spec.name == name.as_str()))
            {
                if config.disabled {
                    continue;
                }
                let mut preamble = config
                    .system_prompts
                    .prelude
                    .as_deref()
                    .unwrap_or(EXTRA_WORKER_PREAMBLE)
                    .to_string();
                if let Some(interlude) = config.system_prompts.interlude.as_deref() {
                    preamble.push_str("\n\n");
                    preamble.push_str(interlude);
                }
                let tool_scene = ToolScene::build(Some(&config.tools));
                let worker_tools = worker_tools(
                    config.toolset.unwrap_or(SubagentToolset::Read),
                    lsp.clone(),
                    locks.clone(),
                    max_output_chars,
                    max_output_bytes,
                    access.clone(),
                    &tool_scene,
                    web_search,
                    office_converter,
                    skills,
                    shell_tx.tagged(name),
                    shell.clone(),
                );
                workers.push(shuvarie_llm::WorkerAgent::new(
                    name,
                    config
                        .description
                        .as_deref()
                        .unwrap_or(EXTRA_WORKER_DESCRIPTION),
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
    }
    WorkerSet { workers, usage }
}

/// A worker's tool set: the `toolset`-selected built-in builders, still
/// filtered through the entry's per-tool overrides.
#[allow(clippy::too_many_arguments)]
fn worker_tools(
    tool_set: SubagentToolset,
    lsp: SharedManager,
    locks: FileLocks,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: crate::permissions::Access,
    tool_scene: &ToolScene,
    web_search: Option<&WebSearchConfig>,
    office_converter: Option<&str>,
    skills: &Skills,
    shell_tx: ShellOutputTx,
    shell: Shell,
) -> Vec<shuvarie_llm::DynamicTool> {
    match tool_set {
        SubagentToolset::Read => tools::read_tools(
            lsp,
            tools::ReadCache::new(),
            max_output_chars,
            max_output_bytes,
            access,
            tool_scene,
            web_search,
            office_converter,
            skills,
        ),
        SubagentToolset::Command => tools::command_tools(shell_tx, shell, access, tool_scene),
        SubagentToolset::Edit => tools::edit_tools(
            lsp,
            locks,
            tools::ReadCache::new(),
            max_output_chars,
            max_output_bytes,
            access,
            tool_scene,
            office_converter,
        ),
        SubagentToolset::None => Vec::new(),
    }
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
    use crate::scenes::CRITIC_PRELUDE;
    use shuvarie_config::SceneConfig;
    use shuvarie_config::ScenesConfig;
    use shuvarie_llm::ProviderKind;

    fn scene(name: &str, config: SceneConfig) -> Scene {
        let mut scenes = ScenesConfig::default();
        scenes.scenes.insert(name.to_string(), config);
        Scene::resolve(&scenes, Some(name))
    }

    fn build(scene: &Scene) -> WorkerSet {
        let (tx, _rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(4);
        build_workers(
            ProviderClient::build(
                ProviderKind::new(selune::ProviderType::Ollama, None),
                None,
                None,
            )
            .unwrap(),
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

    fn builtin_scene(name: &str) -> Scene {
        let set = crate::scenes::builtin_set();
        Scene::resolve(&set, Some(name))
    }

    fn tool_names(tools: Vec<shuvarie_llm::DynamicTool>) -> Vec<String> {
        tools.iter().map(|tool| tool.name().to_string()).collect()
    }

    fn lsp_shared() -> SharedManager {
        std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_lsp::LspManager::new(
            std::path::PathBuf::from("."),
            false,
            Default::default(),
        )))
    }

    #[test]
    fn orchestrator_roster_appends_the_bench() {
        let set = build(&builtin_scene(crate::scenes::ORCHESTRATOR_SCENE_NAME));
        assert_eq!(
            names(&set),
            vec![
                "explore_workspace",
                "run_tests",
                "edit_files",
                "architect",
                "critic",
                "scout",
            ],
            "builtins keep their fixed roster order, extras follow in name order"
        );
        let critic = set
            .workers
            .iter()
            .find(|worker| worker.name() == "critic")
            .expect("critic materializes");
        assert_eq!(
            critic.description(),
            "Planning bench member: stress-test the plan or approach — challenge assumptions, surface failure modes and missing requirements, and gather counter-evidence from the workspace."
        );
        assert_eq!(critic.preamble(), CRITIC_PRELUDE);
    }

    #[test]
    fn extra_workers_materialize_from_the_config() {
        let scene = scene(
            "Solo",
            SceneConfig {
                subagents: shuvarie_config::SubagentsConfig {
                    workers: BTreeMap::from([
                        (
                            "silent".into(),
                            shuvarie_config::SubagentConfig {
                                disabled: true,
                                ..Default::default()
                            },
                        ),
                        ("analyst".into(), shuvarie_config::SubagentConfig::default()),
                        (
                            "historian".into(),
                            shuvarie_config::SubagentConfig {
                                description: Some("The project's memory.".into()),
                                system_prompts: shuvarie_config::SystemPromptsConfig {
                                    prelude: Some("You remember the past.".into()),
                                    ..Default::default()
                                },
                                ..Default::default()
                            },
                        ),
                    ]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let set = build(&scene);
        assert_eq!(
            names(&set),
            vec![
                "explore_workspace",
                "run_tests",
                "edit_files",
                "analyst",
                "historian"
            ],
            "disabled extras are skipped; extras sort by name"
        );
        let historian = &set.workers[4];
        assert_eq!(historian.description(), "The project's memory.");
        assert_eq!(historian.preamble(), "You remember the past.");
        let analyst = &set.workers[3];
        assert_eq!(analyst.description(), EXTRA_WORKER_DESCRIPTION);
        assert_eq!(analyst.preamble(), EXTRA_WORKER_PREAMBLE);
    }

    #[test]
    fn toolset_selects_the_worker_tool_set() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(4);
        let shell_tx = ShellOutputTx::new(tx);
        let gate = |tool_set| {
            tool_names(worker_tools(
                tool_set,
                lsp_shared(),
                FileLocks::new(),
                100,
                100,
                crate::test_util::access(),
                &ToolScene::default(),
                None,
                None,
                &Skills::default(),
                shell_tx.clone(),
                crate::shell::resolve(None).shell,
            ))
        };
        let read = gate(SubagentToolset::Read);
        assert!(read.iter().any(|tool| tool == "read_file"));
        assert!(!read.iter().any(|tool| tool == "run_shell"));
        assert_eq!(
            gate(SubagentToolset::Command),
            vec!["run_shell".to_string()]
        );
        let edit = gate(SubagentToolset::Edit);
        assert!(edit.iter().any(|tool| tool == "apply_patch"));
        assert!(edit.iter().any(|tool| tool == "edit_file"));
        assert!(!edit.iter().any(|tool| tool == "run_shell"));
        assert!(
            gate(SubagentToolset::None).is_empty(),
            "no toolset, no tools"
        );
    }

    #[test]
    fn a_builtin_toolset_override_swaps_the_set() {
        let scene = scene(
            "Planner",
            SceneConfig {
                subagents: shuvarie_config::SubagentsConfig {
                    workers: BTreeMap::from([(
                        "run_tests".into(),
                        shuvarie_config::SubagentConfig {
                            toolset: Some(SubagentToolset::Read),
                            ..Default::default()
                        },
                    )]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let set = build(&scene);
        assert_eq!(
            names(&set),
            vec!["explore_workspace", "run_tests", "edit_files"]
        );
        // The override keeps the name and the description; only the tool set
        // moves — asserted through the selection helper above, since the
        // resolved set is only reachable as dynamic tools at run time.
        assert_eq!(
            set.workers[1].description(),
            "Run the project's build, test, and lint commands, iterate on failures, and report the outcome. Returns a summary of what was run and the final status. Use to verify changes or diagnose failing commands."
        );
    }
}
