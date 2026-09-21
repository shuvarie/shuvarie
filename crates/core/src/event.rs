use shuvarie_db::SessionSummary;
use shuvarie_llm::FileChange;
use shuvarie_llm::{Model, ShellStreams, TokenUsage};

use crate::question::QuestionPrompt;

#[derive(Debug, Clone)]
pub enum Event {
    Pong,
    ModelsLoaded {
        provider_name: String,
        models: Vec<Model>,
    },
    ModelsError {
        provider_name: String,
        error: String,
    },
    /// The hosted registry fetch (on demand) succeeded.
    RegistryLoaded {
        providers: Vec<selune::Provider>,
    },
    /// The hosted registry fetch (on demand) failed.
    RegistryError {
        error: String,
    },
    ConfigSaved,
    ConfigError {
        error: String,
    },
    SessionStarted,
    SessionCreated {
        id: uuid::Uuid,
        title: String,
        scene: Option<String>,
    },
    TokenReceived {
        content: String,
    },
    ReasoningReceived {
        content: String,
    },
    ContextLoaded {
        paths: Vec<String>,
    },
    ToolStarted {
        name: String,
        args: serde_json::Value,
        worker: Option<String>,
        call_id: String,
    },
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
        worker: Option<String>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
        duration_ms: u64,
        call_id: String,
    },
    ToolOutput {
        tool: String,
        worker: Option<String>,
        /// The `run_shell` call this streamed chunk belongs to, resolved by
        /// the core from the chunk's command against the still-running calls;
        /// `None` when ambiguous or already settled, in which case the UI
        /// falls back to the name+worker match.
        call_id: Option<String>,
        stdout: String,
        stderr: String,
    },
    /// A bash-mode (`!`) command started running; display-only, never
    /// persisted or sent to the model. `id` routes the follow-up events.
    BashStarted {
        id: u64,
        command: String,
    },
    /// Live output tails for a running bash-mode command.
    BashOutput {
        id: u64,
        stdout: String,
        stderr: String,
    },
    /// A bash-mode command finished. `exit` is `None` when the process died
    /// to a signal (or failed to spawn, in which case `stdout` carries the
    /// spawn error).
    BashFinished {
        id: u64,
        ok: bool,
        exit: Option<i32>,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    WorkerStarted {
        name: String,
        args: serde_json::Value,
        call_id: String,
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
        duration_ms: u64,
        call_id: String,
    },
    StreamDone {
        text: String,
        usage: TokenUsage,
    },
    /// The agent is busy, so a submitted prompt was queued (steered) instead
    /// of starting a new turn; it will be sent as the next user turn once the
    /// agent finishes its current action (tool call, thinking, or text
    /// segment), or when the turn completes.
    PromptSteered {
        content: String,
    },
    /// A new user turn started streaming: either an accepted `SendMessage`
    /// (`steered: false`) or a dispatched steered prompt (`steered: true`, in
    /// which case the first queued entry must leave the chat display).
    TurnStarted {
        content: String,
        steered: bool,
    },
    /// Reply to [`Command::RecallSteered`]: the recalled prompt content, or
    /// `None` when nothing was queued.
    SteeredRecalled {
        stacked: bool,
        content: Option<String>,
    },
    /// The steered queue was wiped by a session-level transition (new,
    /// loaded, or deleted session).
    SteeredCleared,
    StreamError {
        error: String,
    },
    StreamCancelled,
    /// The context budget overflowed and an LLM compaction summarizer call
    /// is now running. No stream events arrive until the matching
    /// [`Event::CompactionFinished`], so the TUI must keep its busy indicator
    /// armed for the whole window.
    CompactionStarted,
    /// The compaction summarizer call finished (success or failure); the
    /// core task replays the interrupted turn afterwards.
    CompactionFinished,
    /// A retryable connection failure occurred; the core task will re-send the
    /// turn after `delay_ms`. `attempt` is the upcoming retry number (1-based)
    /// out of `max_attempts` (from `[retry].max-retries`).
    RetryScheduled {
        reason: String,
        message: String,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
    },
    /// Per-request usage for one completed LLM request (main stream or
    /// worker), added to the session's running totals. `context_tokens` is
    /// the request's context footprint (see
    /// `shuvarie_llm::context_footprint`) — present only for main-stream
    /// requests, since workers run separate conversations — so the UI can
    /// anchor its context-occupancy display on the latest one.
    UsageUpdate {
        usage: TokenUsage,
        cost: f64,
        context_tokens: Option<u64>,
    },
    /// Authoritative cumulative usage for the active session, sent after a
    /// turn commits. Replaces (rather than adds to) any client-side running
    /// totals so live per-request updates resync to the persisted numbers.
    UsageSnapshot {
        usage: TokenUsage,
        cost: f64,
    },
    SessionsLoaded {
        sessions: Vec<SessionSummary>,
    },
    SessionLoaded {
        id: uuid::Uuid,
        title: String,
        session: crate::Session,
    },
    SessionDeleted {
        id: uuid::Uuid,
    },
    /// A session's title changed: a `/title` edit, or a background
    /// small-model generation replacing the provisional first-prompt
    /// heuristic. The TUI updates its title bar and terminal tab title when
    /// the changed session is the active one.
    SessionTitleChanged {
        id: uuid::Uuid,
        title: String,
    },
    SessionError {
        error: String,
    },
    /// A session could not be entered (or its lock was lost mid-session):
    /// another live client holds the session-wide lock. The session list
    /// refreshes so the picker can show it as in use.
    SessionLocked {
        id: uuid::Uuid,
    },
    /// The session forked: the active path now ends at a different node (an
    /// `/undo` fork before the last user prompt, or a `/tree` fork before a
    /// turn node). Carries the reloaded session plus the forked-away node's
    /// content to recall into the input (`prompt: Some`) or `None` for
    /// marker forks and automatic resumes (replay / interrupted retry).
    Forked {
        session: crate::Session,
        prompt: Option<String>,
    },
    /// Reply to [`Command::OpenTree`]: the freshly loaded session tree for
    /// the popup. Never touches the chat pane's state.
    SessionTree {
        session: crate::Session,
    },
    /// The active session was written to a JSON file (`/export`); `path` is
    /// the file it was written to.
    SessionExported {
        path: std::path::PathBuf,
    },
    SearchResults {
        hits: Vec<shuvarie_db::SearchHit>,
    },
    SearchError {
        error: String,
    },
    QuestionAsked {
        id: u64,
        questions: Vec<QuestionPrompt>,
    },
    /// A tool call hit an `ask` permission rule and is paused until the user
    /// answers; the TUI shows the `description` (the action plus the matched
    /// rule) and replies with [`Command::PermissionDecide`]. `allow_session`
    /// marks asks the user can grant for the rest of the session (paths and
    /// commands; scene confirmations are one-shot).
    PermissionRequested {
        id: u64,
        description: String,
        allow_session: bool,
    },
    LspStatus {
        servers: Vec<shuvarie_lsp::LspStatus>,
    },
    LspDiagnostics {
        path: String,
        diagnostics: Vec<shuvarie_lsp::DiagnosticInfo>,
    },
    LspError {
        error: String,
    },
    /// The state of every configured MCP server: emitted at startup, after
    /// each turn's MCP connect pass, and by [`Command::McpList`] /
    /// [`Command::McpReconnect`]. The sidebar's MCP section renders it.
    McpStatus {
        servers: Vec<shuvarie_mcp::McpStatus>,
    },
    /// An MCP lifecycle action failed (unknown server name, or a connection
    /// attempt that did not complete). The per-server failure detail is
    /// carried by the following [`Event::McpStatus`].
    McpError {
        error: String,
    },
    SkillsLoaded {
        skills: Vec<crate::Skill>,
        warnings: Vec<crate::SkillWarning>,
    },
    /// A startup warning surfaced as a dismissible popup — currently: the
    /// `shell.path` configured in config.kdl was not found, so `run_shell`
    /// falls back to the platform default shell.
    ShellWarning {
        message: String,
    },
    /// An OAuth-backed provider (ChatGPT, Copilot) started a device-code
    /// sign-in: the user must visit `verification_uri` and enter `user_code`
    /// in the browser; the provider completes sign-in automatically while it
    /// polls. Surfaced as a dismissible popup like [`Event::ShellWarning`].
    AuthPrompt {
        provider: String,
        verification_uri: String,
        user_code: String,
    },
    /// An OAuth device-flow sign-in completed: the provider holds a usable
    /// credential (a fresh browser authorization, or a cached/refreshed token
    /// accepted during a [`Command::AuthProviderLogin`] check).
    AuthSuccess {
        provider: String,
    },
    /// An OAuth sign-in failed or the client could not authorize: the device
    /// flow timed out or was declined, the provider does not use OAuth sign-in
    /// (an API key is required), or the connection name is unknown.
    AuthFailed {
        provider: String,
        error: String,
    },
    /// The configured scene set for the scene switcher, sent at startup:
    /// the built-in Default first, then the configured scenes in name order
    /// with their optional descriptions. Entries carry their switch identity
    /// (`SceneListEntry::id`, `None` = built-in Default) rather than bare
    /// names, so a configured scene named "Default" stays switchable.
    /// `default` is the scene new sessions start under (`None` = built-in
    /// Default). `warnings` carries one message per same-level scene
    /// conflict (a name defined by more than one source of the global or
    /// local config loads neither copy).
    ScenesLoaded {
        scenes: Vec<crate::scenes::SceneListEntry>,
        default: Option<String>,
        warnings: Vec<String>,
    },
    /// The active session's scene changed (`None` = built-in Default).
    SceneChanged {
        name: Option<String>,
    },
    /// A scene switch failed: the agent is busy, or the scene name no longer
    /// resolves.
    SceneError {
        error: String,
    },
}
