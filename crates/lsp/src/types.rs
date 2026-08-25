use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl ServerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ServerStatus::Stopped => "stopped",
            ServerStatus::Starting => "starting",
            ServerStatus::Running => "running",
            ServerStatus::Stopping => "stopping",
            ServerStatus::Failed => "failed",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, ServerStatus::Starting | ServerStatus::Running)
    }
}

#[derive(Debug, Clone)]
pub struct LspStatus {
    pub name: String,
    pub language: String,
    pub status: ServerStatus,
    pub pid: Option<u32>,
    pub diagnostics: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl DiagnosticSeverity {
    pub fn from_lsp_types(sev: Option<lsp_types::DiagnosticSeverity>) -> Self {
        use lsp_types::DiagnosticSeverity as S;
        match sev {
            Some(S::ERROR) => DiagnosticSeverity::Error,
            Some(S::WARNING) => DiagnosticSeverity::Warning,
            Some(S::INFORMATION) => DiagnosticSeverity::Information,
            Some(S::HINT) => DiagnosticSeverity::Hint,
            _ => DiagnosticSeverity::Information,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
            DiagnosticSeverity::Information => "info",
            DiagnosticSeverity::Hint => "hint",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiagnosticInfo {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub severity: DiagnosticSeverity,
    pub source: Option<String>,
    pub code: Option<String>,
    pub message: String,
}

impl DiagnosticInfo {
    pub fn from_lsp_types(d: &lsp_types::Diagnostic) -> Self {
        let line = d.range.start.line.saturating_add(1).max(1);
        let col = d.range.start.character.saturating_add(1).max(1);
        let end_line = d.range.end.line.saturating_add(1).max(1);
        let end_col = d.range.end.character.saturating_add(1).max(1);
        DiagnosticInfo {
            line,
            col,
            end_line,
            end_col,
            severity: DiagnosticSeverity::from_lsp_types(d.severity),
            source: d.source.clone(),
            code: d.code.as_ref().map(|c| format!("{c:?}")),
            message: d.message.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DiagnosticUpdate {
    pub path: String,
    pub diagnostics: Vec<DiagnosticInfo>,
}

pub type DiagnosticMap = BTreeMap<String, Vec<DiagnosticInfo>>;
