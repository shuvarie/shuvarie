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
    SaveConfig,
    StartSession,
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
    UndoLastTurn,
    Redo,
    Replay,
    Continue,
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
