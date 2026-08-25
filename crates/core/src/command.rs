use crate::connections::ProviderConfig;

#[derive(Debug, Clone)]
pub enum Command {
    Ping,
    ListModels {
        provider_name: String,
    },
    AddProvider {
        name: String,
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
    CancelStream,
    ListSessions,
    LoadSession {
        id: u64,
    },
    DeleteSession {
        id: u64,
    },
    SearchHistory {
        query: String,
    },
    ApproveTool {
        id: u64,
        approved: bool,
        always: bool,
    },
    UndoLastTurn,
    Redo,
    Replay,
    Resume,
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
}
