use std::sync::Arc;

use shuvarie_catalog::TokenUsage;
use shuvarie_llm::ProviderClient;

use crate::approval::ApprovalGate;
use crate::lsp_manager::SharedManager;
use crate::tools;

pub struct WorkerSet {
    pub workers: Vec<shuvarie_llm::WorkerAgent>,
    pub usage: Arc<std::sync::Mutex<TokenUsage>>,
}

pub fn build_workers(
    client: ProviderClient,
    model: &str,
    gate: ApprovalGate,
    lsp: SharedManager,
    worker_max_turns: usize,
) -> WorkerSet {
    let usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let workers = vec![
        shuvarie_llm::WorkerAgent::new(
            "explore_workspace",
            "Explore the workspace: list directories, read files, and grep for text to locate, understand, and summarize code. Returns a concise report with file paths and line numbers. Use when a task requires understanding existing code before changes.",
            EXPLORER_PREAMBLE,
            client.clone(),
            model,
            tools::read_tools(gate.clone(), lsp.clone()),
            Arc::clone(&usage),
            worker_max_turns,
        ),
        shuvarie_llm::WorkerAgent::new(
            "run_tests",
            "Run the project's build, test, and lint commands, iterate on failures, and report the outcome. Returns a summary of what was run and the final status. Use to verify changes or diagnose failing commands.",
            TESTER_PREAMBLE,
            client.clone(),
            model,
            tools::command_tools(gate.clone()),
            Arc::clone(&usage),
            worker_max_turns,
        ),
        shuvarie_llm::WorkerAgent::new(
            "edit_files",
            "Read, write, and edit files to implement changes in the workspace. Returns a summary of the files changed and what was done. Use when the task requires modifying code or other files.",
            EDITOR_PREAMBLE,
            client,
            model,
            tools::edit_tools(gate, lsp),
            Arc::clone(&usage),
            worker_max_turns,
        ),
    ];
    WorkerSet { workers, usage }
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
the target files first to understand the existing code before editing. After \
finishing, summarize the files you changed and what you did in a short reply.";
