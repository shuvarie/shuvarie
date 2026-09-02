use shuvarie_db::StoredToolCall;
use shuvarie_llm::FileChange;

#[derive(Debug, Clone)]
pub struct ToolRecord {
    pub name: String,
    pub args_json: String,
    pub output: String,
    pub stderr: String,
    pub ok: bool,
    pub worker: Option<String>,
    pub message_id: u64,
    pub message_seq: u64,
    pub file_change: Option<FileChange>,
    pub original_content: Option<String>,
    pub new_content: Option<String>,
    pub duration_ms: u64,
}

impl ToolRecord {
    pub fn from_stored(tc: StoredToolCall) -> Self {
        let file_change = if tc.file_change_json.is_empty() {
            None
        } else {
            serde_json::from_str::<FileChange>(&tc.file_change_json).ok()
        };
        Self {
            name: tc.name,
            args_json: tc.args_json,
            output: tc.output,
            stderr: tc.stderr,
            ok: tc.ok,
            worker: tc.worker,
            message_id: tc.message_id,
            message_seq: tc.seq,
            file_change,
            original_content: tc.original_content,
            new_content: tc.new_content,
            duration_ms: tc.duration_ms,
        }
    }
}
