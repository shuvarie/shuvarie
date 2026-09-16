//! The on-disk session interchange format (`shuvarie-db::SessionFile`): a
//! versioned JSON snapshot of one session — its row, the full message tree,
//! the tool calls, and the scroll position. Written by `shuvarie
//! --export-session` and the `/export` command, restored by
//! `shuvarie --import-session`. Message embeddings are not part of the file;
//! they are regenerated after an import.

use jiff::Timestamp;
use shuvarie_llm::TokenUsage;

use crate::error::{DbError, Result};
use crate::model::{MsgRole, ReasoningSegment, TextSegment};
use crate::store::{StoredMessage, StoredScroll, StoredSession, StoredToolCall};

pub const FILE_FORMAT: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionFile {
    pub format: u32,
    pub session: FileSession,
    pub messages: Vec<FileMessage>,
    #[serde(default)]
    pub tool_calls: Vec<FileToolCall>,
    /// The chat pane's scroll position (`turn` is a dense index into the
    /// active path, so it survives the import's id remapping unchanged).
    #[serde(default)]
    pub scroll: StoredScroll,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileSession {
    pub id: uuid::Uuid,
    pub title: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub scene: Option<String>,
    /// Message id of the active branch's tip.
    #[serde(default)]
    pub leaf_id: Option<u64>,
    /// Session row timestamps, epoch milliseconds; `None` = import with the
    /// current time.
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileMessage {
    pub id: u64,
    /// Parent message id in the session tree; `None` for root prompts.
    #[serde(default)]
    pub parent_id: Option<u64>,
    pub seq: u64,
    pub role: MsgRole,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub reasoning: Vec<ReasoningSegment>,
    #[serde(default)]
    pub text_segments: Vec<TextSegment>,
    #[serde(default)]
    pub interrupted: bool,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub summary: bool,
    /// Usage of the turn's last main-stream request.
    #[serde(default)]
    pub request: TokenUsage,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileToolCall {
    pub message_id: u64,
    pub seq: u64,
    pub name: String,
    #[serde(default)]
    pub args_json: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub killed: bool,
    #[serde(default)]
    pub worker: Option<String>,
    #[serde(default)]
    pub file_change_json: String,
    #[serde(default)]
    pub original_content: Option<String>,
    #[serde(default)]
    pub new_content: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
}

impl SessionFile {
    pub fn from_stored(stored: &StoredSession) -> Self {
        Self {
            format: FILE_FORMAT,
            session: FileSession {
                id: stored.id,
                title: stored.title.clone(),
                provider: stored.provider.clone(),
                model: stored.model.clone(),
                scene: stored.scene.clone(),
                leaf_id: stored.leaf_id,
                created_at: Some(stored.created_at.as_millisecond()),
                updated_at: Some(stored.updated_at.as_millisecond()),
            },
            messages: stored.messages.iter().map(FileMessage::from).collect(),
            tool_calls: stored.tool_calls.iter().map(FileToolCall::from).collect(),
            scroll: stored.scroll,
        }
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| DbError::SessionFile(format!("serialize: {e}")))
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let file: Self =
            serde_json::from_str(json).map_err(|e| DbError::SessionFile(format!("parse: {e}")))?;
        if file.format != FILE_FORMAT {
            return Err(DbError::SessionFile(format!(
                "unsupported format {} (expected {FILE_FORMAT})",
                file.format
            )));
        }
        Ok(file)
    }

    pub fn write_json(&self, path: &std::path::Path) -> Result<()> {
        let json = self.to_json()?;
        std::fs::write(path, json.as_bytes())
            .map_err(|e| DbError::SessionFile(format!("write {}: {e}", path.display())))
    }
}

/// The export destination: `path` when given (with the default file name
/// joined under it when it is a directory), else the default file name in the
/// working directory.
pub fn resolve_export_path(
    path: Option<&std::path::Path>,
    session_id: uuid::Uuid,
) -> std::path::PathBuf {
    match path {
        Some(path) if path.is_dir() => path.join(default_file_name(session_id)),
        Some(path) => path.to_path_buf(),
        None => std::path::PathBuf::from(default_file_name(session_id)),
    }
}

/// `<session_id>-<YYYYMMDD_HHMMSS>.json`, the name used when no export path
/// is given.
pub fn default_file_name(session_id: uuid::Uuid) -> String {
    let stamp = jiff::Zoned::now().strftime("%Y%m%d_%H%M%S").to_string();
    format!("{session_id}-{stamp}.json")
}

impl From<&StoredMessage> for FileMessage {
    fn from(m: &StoredMessage) -> Self {
        Self {
            id: m.id,
            parent_id: m.parent_id,
            seq: m.seq,
            role: m.role,
            content: m.content.clone(),
            reasoning: m.reasoning.clone(),
            text_segments: m.text_segments.clone(),
            interrupted: m.interrupted,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            total_tokens: m.total_tokens,
            cached_input_tokens: m.cached_input_tokens,
            reasoning_tokens: m.reasoning_tokens,
            cost: m.cost,
            summary: m.summary,
            request: m.request,
        }
    }
}

impl From<&StoredToolCall> for FileToolCall {
    fn from(t: &StoredToolCall) -> Self {
        Self {
            message_id: t.message_id,
            seq: t.seq,
            name: t.name.clone(),
            args_json: t.args_json.clone(),
            output: t.output.clone(),
            stderr: t.stderr.clone(),
            ok: t.ok,
            killed: t.killed,
            worker: t.worker.clone(),
            file_change_json: t.file_change_json.clone(),
            original_content: t.original_content.clone(),
            new_content: t.new_content.clone(),
            duration_ms: t.duration_ms,
        }
    }
}

/// The JSON-encodable session timestamps, back from epoch milliseconds.
pub(crate) fn timestamp_from_millis(ms: Option<i64>, fallback: Timestamp) -> Timestamp {
    ms.and_then(|ms| Timestamp::from_millisecond(ms).ok())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_name_has_id_and_timestamp_shape() {
        let id = uuid::Uuid::now_v7();
        let name = default_file_name(id);
        let stamp = name
            .strip_prefix(&format!("{id}-"))
            .and_then(|rest| rest.strip_suffix(".json"))
            .expect("name is <id>-<stamp>.json");
        assert_eq!(stamp.len(), 15, "YYYYMMDD_HHMMSS: {stamp}");
        assert_eq!(stamp.as_bytes()[8], b'_');
        assert!(stamp.chars().all(|c| c.is_ascii_digit() || c == '_'));
    }

    #[test]
    fn round_trips_through_json_losing_nothing() {
        let json = r#"{
            "format": 1,
            "session": {
                "id": "019370ff-2f52-7000-8000-000000000000",
                "title": "t",
                "provider": null,
                "model": "m",
                "scene": null,
                "leaf_id": 2,
                "created_at": 1732100000000,
                "updated_at": 1732100500000
            },
            "messages": [
                {
                    "id": 1, "parent_id": null, "seq": 0, "role": "user",
                    "content": "hi"
                },
                {
                    "id": 2, "parent_id": 1, "seq": 1, "role": "assistant",
                    "content": "ok", "summary": false,
                    "input_tokens": 3, "output_tokens": 4
                }
            ],
            "tool_calls": [
                {
                    "message_id": 2, "seq": 0, "name": "read_file",
                    "args_json": "{}", "output": "x", "ok": true
                }
            ],
            "scroll": { "sticky": false, "anchor": [1, 2] }
        }"#;
        let file = SessionFile::from_json(json).unwrap();
        assert_eq!(file.session.title, "t");
        assert_eq!(file.session.model.as_deref(), Some("m"));
        assert_eq!(file.messages.len(), 2);
        assert_eq!(file.messages[0].role, MsgRole::User);
        assert_eq!(file.messages[1].parent_id, Some(1));
        assert_eq!(file.tool_calls.len(), 1);
        assert_eq!(file.tool_calls[0].name, "read_file");
        assert_eq!(file.scroll.anchor, Some((1, 2)));

        let round = SessionFile::from_json(&file.to_json().unwrap()).unwrap();
        assert_eq!(round.session.id, file.session.id);
        assert_eq!(round.messages.len(), file.messages.len());
        assert_eq!(round.tool_calls.len(), file.tool_calls.len());
    }

    #[test]
    fn from_json_rejects_unknown_format() {
        let err = SessionFile::from_json(r#"{"format": 99, "session": {"id": "019370ff-2f52-7000-8000-000000000000", "title": "t"}, "messages": []}"#)
            .unwrap_err();
        assert!(
            err.to_string().contains("unsupported format 99"),
            "unexpected error: {err}"
        );
    }
}
