use crate::config::ProviderConfig;

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
    CancelStream,
    ListSessions,
    LoadSession {
        id: uuid::Uuid,
    },
    DeleteSession {
        id: uuid::Uuid,
    },
    SearchHistory {
        query: String,
    },
    ApproveTool {
        id: u64,
        approved: bool,
        always: bool,
    },
    AnswerQuestion {
        id: u64,
        /// `None` when the user dismissed the question.
        answers: Option<Vec<Vec<String>>>,
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
