use shuvarie_config::ProviderConfig;
use shuvarie_db::StoredScroll;

#[derive(Debug, Clone)]
pub enum Command {
    Ping,
    ListModels {
        provider_name: String,
    },
    AddProvider {
        id: String,
        config: ProviderConfig,
    },
    RemoveProvider {
        name: String,
    },
    SetActiveProvider {
        name: String,
    },
    SetActiveModel {
        model: String,
    },
    /// Cycle the active model's reasoning-effort variant (Ctrl+T): forward
    /// through the Selune catalog's variants, wrapping past the last back to
    /// the unset default. A no-op when the active model is not in the catalog.
    CycleVariant,
    /// Set the active model's reasoning-effort variant directly
    /// (`/variant [name]`). The TUI validates the value against the catalog
    /// before sending; `None` selects the unset default.
    SelectVariant {
        variant: Option<String>,
    },
    SaveConfig,
    NewSession,
    SendMessage {
        content: String,
    },
    /// Run a bash-mode (`!`-prefixed) command through the resolved shell.
    /// Display-only: the output never persists or reaches the model.
    RunBash {
        command: String,
    },
    CancelStream,
    ListSessions,
    LoadSession {
        id: uuid::Uuid,
    },
    /// Persist the chat pane's last scroll position for a session, sent by
    /// the TUI when it is about to leave the session (switch or quit).
    SaveScroll {
        id: uuid::Uuid,
        scroll: StoredScroll,
    },
    DeleteSession {
        id: uuid::Uuid,
    },
    /// Rename the active session. The core trims the title; an empty (or
    /// unchanged) title is a no-op.
    SetTitle {
        title: String,
    },
    SearchHistory {
        query: String,
    },
    AnswerQuestion {
        id: u64,
        /// `None` when the user dismissed the question.
        answers: Option<Vec<Vec<String>>>,
    },
    /// The user's answer to a pending `ask` permission prompt (`id` from
    /// [`crate::Event::PermissionRequested`]). A missing id is a no-op.
    PermissionDecide {
        id: u64,
        allow: bool,
    },
    /// Fork the session: with `node: None`, walk to the active path's last
    /// user prompt (`/undo`); with a node id, walk to that node. Every turn
    /// node forks *before* itself — its parent becomes the tip and its
    /// content is recalled into the input — while summary/system nodes walk
    /// to themselves. When `summarize`, an LLM summary of the prefix before
    /// the fork point is created first and the forked-away node is
    /// reparented under it.
    ForkSession {
        node: Option<u64>,
        summarize: bool,
    },
    /// Delete a branch of the session tree: the node and all of its
    /// descendants (must not contain the active leaf).
    DeleteBranch {
        node: u64,
    },
    /// Load the active session's tree for the `/tree` popup; reports
    /// [`crate::Event::SessionTree`].
    OpenTree,
    /// Write the active session to a JSON file (`/export [path]`); `None`
    /// picks `<session_id>-<timestamp>.json` in the working directory.
    /// Reports [`crate::Event::SessionExported`] or
    /// [`crate::Event::SessionError`].
    ExportSession {
        path: Option<std::path::PathBuf>,
    },
    /// Switch the active session's scene (`None` = the built-in default
    /// scene). Refused with [`crate::Event::SceneError`] while a stream is
    /// busy or the name does not resolve.
    SwitchScene {
        name: Option<String>,
    },
    Replay,
    Reload,
    /// Recall the most recently steered prompt into the input area. `stacked`
    /// (Alt+Shift+Up) prepends the content to the existing text (separated by
    /// two line feeds) instead of overwriting it.
    RecallSteered {
        stacked: bool,
    },
    LspStart {
        name: String,
    },
    LspStop {
        name: String,
    },
    LspRestart {
        name: String,
    },
    LspList {
        all: bool,
        filter: Option<String>,
    },
    /// Fetch the hosted (Selune) provider registry on demand. Reports
    /// [`Event::RegistryLoaded`] or [`Event::RegistryError`].
    FetchRegistry,
}
