use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shuvarie_llm::Role;
use shuvarie_llm::TokenUsage;
use toasty::db::Driver;
use toasty::schema::db;
use toasty::stmt::Type;

use crate::error::{DbError, Result};
use crate::model::{
    Message, MessageEmbedding, MsgRole, ReasoningSegment, Session, SessionType, TextSegment,
    ToolCall, encode_reasoning, encode_text_segments, parse_reasoning, parse_text_segments,
};
use crate::session_file::{FileMessage, SessionFile, timestamp_from_millis};

static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

pub use shuvarie_config::WORKSPACE_DIR_NAME;

pub const SESSION_LOCK_TTL_MS: i64 = 30_000;

pub const SESSION_LOCK_HEARTBEAT_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAcquire {
    Acquired,
    Ours,
    Held,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: uuid::Uuid,
    pub title: String,
    pub message_count: u64,
    pub updated_at_epoch_ms: i64,
    pub in_use: bool,
}

/// Persisted chat scroll position of a session: whether the viewport was
/// pinned to the bottom of the history, and — when released from the bottom —
/// the content anchor it was held at (`turn` = dense message index, `row` =
/// wrapped row within that turn).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredScroll {
    pub sticky: bool,
    pub anchor: Option<(u64, u64)>,
}

#[derive(Debug, Clone)]
pub struct StoredSession {
    pub id: uuid::Uuid,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The session's active scene; `None` = the built-in default scene.
    pub scene: Option<String>,
    /// Message id of the active branch's tip; the parent chain from it up to
    /// the root is the active path.
    pub leaf_id: Option<u64>,
    pub messages: Vec<StoredMessage>,
    pub tool_calls: Vec<StoredToolCall>,
    pub scroll: StoredScroll,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: u64,
    /// Parent message id in the session tree; `None` for root prompts.
    pub parent_id: Option<u64>,
    pub role: MsgRole,
    pub content: String,
    pub reasoning: Vec<ReasoningSegment>,
    /// The turn's text runs with the tool-call positions they streamed at, so
    /// a reload rebuilds the interleave; empty on older rows (the runs then
    /// fall back to the joined `content`).
    pub text_segments: Vec<TextSegment>,
    pub interrupted: bool,
    pub seq: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub summary: bool,
    /// Usage of the turn's last main-stream request, parsed from
    /// `request_json`; all-zero when the row has none.
    pub request: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct StoredToolCall {
    pub id: u64,
    pub message_id: u64,
    pub session_id: uuid::Uuid,
    pub seq: u64,
    pub name: String,
    pub args_json: String,
    pub output: String,
    pub stderr: String,
    pub ok: bool,
    /// The call was cut off before it could finish (turn interrupted).
    pub killed: bool,
    pub worker: Option<String>,
    pub file_change_json: String,
    pub original_content: Option<String>,
    pub new_content: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    Fts,
    Semantic,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub message_id: u64,
    pub session_id: uuid::Uuid,
    pub seq: u64,
    pub role: MsgRole,
    pub content: String,
    pub session_title: String,
    pub score: f64,
    pub source: SearchSource,
}

#[derive(Debug, Clone)]
pub struct EmbeddableMessage {
    pub id: u64,
    pub session_id: uuid::Uuid,
    pub seq: u64,
    pub content: String,
}

impl From<Message> for StoredMessage {
    fn from(m: Message) -> Self {
        Self {
            id: m.id,
            parent_id: m.parent_id,
            role: m.role,
            content: m.content,
            reasoning: parse_reasoning(&m.reasoning),
            text_segments: parse_text_segments(&m.text_segments),
            interrupted: m.interrupted,
            seq: m.seq,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            total_tokens: m.total_tokens,
            cached_input_tokens: m.cached_input_tokens,
            reasoning_tokens: m.reasoning_tokens,
            cost: m.cost,
            summary: m.summary,
            request: serde_json::from_str(&m.request_json).unwrap_or_default(),
        }
    }
}

impl From<ToolCall> for StoredToolCall {
    fn from(t: ToolCall) -> Self {
        Self {
            id: t.id,
            message_id: t.message_id,
            session_id: t.session_id,
            seq: t.seq,
            name: t.name,
            args_json: t.args_json,
            output: t.output,
            stderr: t.stderr,
            ok: t.ok,
            killed: t.killed,
            worker: t.worker,
            file_change_json: t.file_change_json,
            original_content: t.original_content,
            new_content: t.new_content,
            duration_ms: t.duration_ms,
        }
    }
}

#[derive(Clone)]
pub struct Store {
    db: toasty::Db,
    client_id: Option<Arc<str>>,
}

impl Store {
    pub fn default_path() -> PathBuf {
        PathBuf::from(WORKSPACE_DIR_NAME).join("data.db")
    }

    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let created = !parent.exists();
            std::fs::create_dir_all(parent)
                .map_err(|e| DbError::Open(format!("create dir {}: {e}", parent.display())))?;
            if created && parent.file_name() == Some(std::ffi::OsStr::new(WORKSPACE_DIR_NAME)) {
                let gitignore = parent.join(".gitignore");
                std::fs::write(&gitignore, "*\n")
                    .map_err(|e| DbError::Open(format!("write {}: {e}", gitignore.display())))?;
            }
        }
        let driver = toasty_driver_turso::Turso::file(path)
            .experimental_index_method(true)
            .experimental_multiprocess_wal(true);
        Self::open_with_driver(driver).await
    }

    pub async fn open_in_memory() -> Result<Self> {
        Self::open_with_driver(
            toasty_driver_turso::Turso::in_memory().experimental_index_method(true),
        )
        .await
    }

    async fn open_with_driver(driver: impl Driver) -> Result<Self> {
        let db = toasty::Db::builder()
            .models(toasty::models!(
                Session,
                Message,
                MessageEmbedding,
                ToolCall
            ))
            .build(driver)
            .await
            .map_err(|e| DbError::Open(e.to_string()))?;
        MIGRATIONS
            .apply(&db)
            .await
            .map_err(|e| DbError::Migration(e.to_string()))?;
        let mut store = Self {
            db,
            client_id: None,
        };
        store.ensure_session_locks().await?;
        Ok(store)
    }

    pub fn with_client_id(mut self, client_id: impl Into<Arc<str>>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }

    pub async fn list_sessions(&mut self) -> Result<Vec<SessionSummary>> {
        let locked = self.locked_session_ids(now_ms()).await?;
        let sessions = Session::filter(Session::fields().session_type().eq(SessionType::Main))
            .latest_by(Session::fields().updated_at())
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;

        let mut out = Vec::with_capacity(sessions.len());
        for s in sessions {
            let count = self
                .message_count(s.id)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
            out.push(SessionSummary {
                id: s.id,
                title: s.title,
                message_count: count,
                updated_at_epoch_ms: s.updated_at.as_millisecond(),
                in_use: locked.contains(&s.id),
            });
        }
        Ok(out)
    }

    pub async fn load_session(&mut self, id: uuid::Uuid) -> Result<StoredSession> {
        let session = Session::filter_by_id(id)
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?
            .ok_or(DbError::NotFound { id })?;
        let messages = self.messages_for_session(id).await?;
        let tool_calls = self.tool_calls_for_session(id).await?;
        let scroll = scroll_of(&session);
        Ok(StoredSession {
            id: session.id,
            title: session.title,
            provider: session.provider,
            model: session.model,
            scene: session.scene,
            leaf_id: session.leaf_id,
            messages,
            tool_calls,
            scroll,
            created_at: session.created_at,
            updated_at: session.updated_at,
        })
    }

    pub async fn most_recent_session(&mut self) -> Result<Option<StoredSession>> {
        let latest = Session::filter(Session::fields().session_type().eq(SessionType::Main))
            .latest_by(Session::fields().updated_at())
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        let Some(session) = latest else {
            return Ok(None);
        };
        let messages = self.messages_for_session(session.id).await?;
        let tool_calls = self.tool_calls_for_session(session.id).await?;
        let scroll = scroll_of(&session);
        Ok(Some(StoredSession {
            id: session.id,
            title: session.title,
            provider: session.provider,
            model: session.model,
            scene: session.scene,
            leaf_id: session.leaf_id,
            messages,
            tool_calls,
            scroll,
            created_at: session.created_at,
            updated_at: session.updated_at,
        }))
    }

    pub async fn create_session(
        &mut self,
        title: &str,
        provider: Option<&str>,
        model: Option<&str>,
        scene: Option<&str>,
    ) -> Result<uuid::Uuid> {
        self.insert_session(title, provider, model, scene, SessionType::Main, None)
            .await
    }

    pub async fn create_worker_session(
        &mut self,
        title: &str,
        provider: Option<&str>,
        model: Option<&str>,
        parent_id: uuid::Uuid,
    ) -> Result<uuid::Uuid> {
        self.insert_session(
            title,
            provider,
            model,
            None,
            SessionType::Worker,
            Some(parent_id),
        )
        .await
    }

    async fn insert_session(
        &mut self,
        title: &str,
        provider: Option<&str>,
        model: Option<&str>,
        scene: Option<&str>,
        session_type: SessionType,
        parent_id: Option<uuid::Uuid>,
    ) -> Result<uuid::Uuid> {
        let session = toasty::create!(Session {
            title: title.to_string(),
            provider: provider.map(|p| p.to_string()),
            model: model.map(|m| m.to_string()),
            scene: scene.map(|s| s.to_string()),
            session_type,
            parent_id,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(session.id)
    }

    pub async fn append_message(
        &mut self,
        session_id: uuid::Uuid,
        parent_id: Option<u64>,
        role: Role,
        content: &str,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            parent_id,
            seq,
            role: MsgRole::from(role),
            content: content.to_string(),
            reasoning: String::new(),
            text_segments: String::new(),
            interrupted: false,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: false,
            request_json: String::new(),
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn append_assistant_message(
        &mut self,
        session_id: uuid::Uuid,
        parent_id: Option<u64>,
        content: &str,
        reasoning: &[ReasoningSegment],
        text_segments: &[TextSegment],
        interrupted: bool,
        usage: TokenUsage,
        cost: f64,
        request: &TokenUsage,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            parent_id,
            seq,
            role: MsgRole::Assistant,
            content: content.to_string(),
            reasoning: encode_reasoning(reasoning),
            text_segments: encode_text_segments(text_segments),
            interrupted,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost,
            summary: false,
            request_json: serde_json::to_string(request).unwrap_or_default(),
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_message(
        &mut self,
        message_id: u64,
        content: &str,
        reasoning: &[ReasoningSegment],
        text_segments: &[TextSegment],
        interrupted: bool,
        usage: TokenUsage,
        cost: f64,
        request: &TokenUsage,
    ) -> Result<()> {
        Message::update_by_id(message_id)
            .content(content.to_string())
            .reasoning(encode_reasoning(reasoning))
            .text_segments(encode_text_segments(text_segments))
            .interrupted(interrupted)
            .input_tokens(usage.input_tokens)
            .output_tokens(usage.output_tokens)
            .total_tokens(usage.total_tokens)
            .cached_input_tokens(usage.cached_input_tokens)
            .reasoning_tokens(usage.reasoning_tokens)
            .cost(cost)
            .request_json(serde_json::to_string(request).unwrap_or_default())
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn append_summary(
        &mut self,
        session_id: uuid::Uuid,
        parent_id: Option<u64>,
        content: &str,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            parent_id,
            seq,
            role: MsgRole::Assistant,
            content: content.to_string(),
            reasoning: String::new(),
            text_segments: String::new(),
            interrupted: false,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: true,
            request_json: String::new(),
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    /// Restore a [`SessionFile`] as a new session row: the file's session id
    /// is kept when free (else a fresh UUID v7 is minted), message and tool
    /// call ids are regenerated (original ids remap to the new parents), the
    /// leaf and scroll carry over, and the row timestamps are preserved.
    /// Imports as a `Main` session regardless of the source's shape.
    pub async fn import_session(&mut self, file: &SessionFile) -> Result<uuid::Uuid> {
        let mut id = file.session.id;
        if self.session_row_exists(id).await? {
            id = uuid::Uuid::now_v7();
        }

        let now = jiff::Timestamp::now();
        let created_at = timestamp_from_millis(file.session.created_at, now);
        let updated_at = timestamp_from_millis(file.session.updated_at, created_at);
        toasty::create!(Session {
            id,
            title: file.session.title.clone(),
            provider: file.session.provider.clone(),
            model: file.session.model.clone(),
            scene: file.session.scene.clone(),
            session_type: SessionType::Main,
            parent_id: None,
            created_at,
            updated_at,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;

        if let Err(e) = self.import_messages(id, file).await {
            self.delete_imported_rows(id).await;
            return Err(e);
        }
        Ok(id)
    }

    async fn session_row_exists(&mut self, id: uuid::Uuid) -> Result<bool> {
        Ok(Session::filter_by_id(id)
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?
            .is_some())
    }

    /// Best-effort cleanup when an import fails partway through its inserts.
    async fn delete_imported_rows(&mut self, session_id: uuid::Uuid) {
        for sql in [
            "DELETE FROM tool_calls WHERE session_id = ?1",
            "DELETE FROM messages WHERE session_id = ?1",
            "DELETE FROM sessions WHERE id = ?1",
        ] {
            let _ = toasty::sql::statement(sql)
                .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
                .exec(&mut self.db)
                .await;
        }
    }

    async fn import_messages(&mut self, session_id: uuid::Uuid, file: &SessionFile) -> Result<()> {
        let mut messages: Vec<&FileMessage> = file.messages.iter().collect();
        messages.sort_by_key(|m| m.seq);
        let mut id_map: HashMap<u64, u64> = HashMap::new();
        for m in messages {
            let parent_id = m.parent_id.and_then(|p| id_map.get(&p).copied());
            let msg = toasty::create!(Message {
                session_id,
                parent_id,
                seq: m.seq,
                role: m.role,
                content: m.content.clone(),
                reasoning: encode_reasoning(&m.reasoning),
                text_segments: encode_text_segments(&m.text_segments),
                interrupted: m.interrupted,
                input_tokens: m.input_tokens,
                output_tokens: m.output_tokens,
                total_tokens: m.total_tokens,
                cached_input_tokens: m.cached_input_tokens,
                reasoning_tokens: m.reasoning_tokens,
                cost: m.cost,
                summary: m.summary,
                request_json: serde_json::to_string(&m.request).unwrap_or_default(),
            })
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
            id_map.insert(m.id, msg.id);
        }

        for tc in &file.tool_calls {
            let Some(&message_id) = id_map.get(&tc.message_id) else {
                continue;
            };
            toasty::create!(ToolCall {
                session_id,
                message_id,
                seq: tc.seq,
                name: tc.name.clone(),
                args_json: tc.args_json.clone(),
                output: tc.output.clone(),
                stderr: tc.stderr.clone(),
                ok: tc.ok,
                killed: tc.killed,
                worker: tc.worker.clone(),
                file_change_json: tc.file_change_json.clone(),
                original_content: tc.original_content.clone(),
                new_content: tc.new_content.clone(),
                duration_ms: tc.duration_ms,
            })
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        }

        let leaf_id = file
            .session
            .leaf_id
            .and_then(|leaf| id_map.get(&leaf).copied());
        self.set_active_leaf(session_id, leaf_id).await?;
        self.set_scroll(session_id, file.scroll).await?;
        Ok(())
    }

    /// Point the session's active branch at a (possibly new) tip. A raw
    /// update bypasses the model's auto-timestamp, so `updated_at` is
    /// untouched.
    pub async fn set_active_leaf(&mut self, id: uuid::Uuid, leaf_id: Option<u64>) -> Result<()> {
        toasty::sql::statement("UPDATE sessions SET leaf_id = ?1 WHERE id = ?2")
            .bind_typed(leaf_id, db::Type::UnsignedInteger(8))
            .bind_typed(id.as_bytes().to_vec(), db::Type::Blob)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    /// Reparent a message under a new parent (branch building for forks and
    /// compaction summaries).
    pub async fn set_message_parent(&mut self, message_id: u64, parent: Option<u64>) -> Result<()> {
        Message::update_by_id(message_id)
            .parent_id(parent)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn set_interrupted(&mut self, message_id: u64, interrupted: bool) -> Result<()> {
        Message::update_by_id(message_id)
            .interrupted(interrupted)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn append_tool_call(
        &mut self,
        session_id: uuid::Uuid,
        message_id: u64,
        seq: u64,
        name: &str,
        args_json: &str,
        output: &str,
        stderr: &str,
        ok: bool,
        killed: bool,
        worker: Option<&str>,
        file_change_json: &str,
        original_content: Option<&str>,
        new_content: Option<&str>,
        duration_ms: u64,
    ) -> Result<u64> {
        let tc = toasty::create!(ToolCall {
            session_id,
            message_id,
            seq,
            name: name.to_string(),
            args_json: args_json.to_string(),
            output: output.to_string(),
            stderr: stderr.to_string(),
            ok,
            killed,
            worker: worker.map(|s| s.to_string()),
            file_change_json: file_change_json.to_string(),
            original_content: original_content.map(|s| s.to_string()),
            new_content: new_content.map(|s| s.to_string()),
            duration_ms,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(tc.id)
    }

    pub async fn tool_calls_for_message(&mut self, message_id: u64) -> Result<Vec<StoredToolCall>> {
        let rows = ToolCall::filter_by_message_id(message_id)
            .order_by(ToolCall::fields().seq().asc())
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(rows.into_iter().map(StoredToolCall::from).collect())
    }

    async fn tool_calls_for_session(
        &mut self,
        session_id: uuid::Uuid,
    ) -> Result<Vec<StoredToolCall>> {
        let rows = ToolCall::filter_by_session_id(session_id)
            .order_by(ToolCall::fields().seq().asc())
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(rows.into_iter().map(StoredToolCall::from).collect())
    }

    /// Delete a branch: the message `root_id` and all of its descendants,
    /// together with their tool calls and embeddings. Returns the deleted
    /// message ids. The caller must ensure the active leaf is not inside the
    /// subtree.
    pub async fn delete_branch(
        &mut self,
        session_id: uuid::Uuid,
        root_id: u64,
    ) -> Result<Vec<u64>> {
        let rows = toasty::sql::query("SELECT id, parent_id FROM messages WHERE session_id = ?1")
            .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
            .column_types([Type::I64, Type::I64])
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in rows {
            let toasty::stmt::Value::Record(fields) = row else {
                continue;
            };
            let get_u64 = |i: usize| -> Option<u64> {
                match &fields[i] {
                    toasty::stmt::Value::I64(v) => Some(*v as u64),
                    toasty::stmt::Value::U64(v) => Some(*v),
                    _ => None,
                }
            };
            if let (Some(id), parent) = (get_u64(0), get_u64(1)) {
                children.entry(parent.unwrap_or(0)).or_default().push(id);
            }
        }
        let mut subtree = Vec::new();
        let mut queue = vec![root_id];
        while let Some(id) = queue.pop() {
            subtree.push(id);
            if let Some(kids) = children.remove(&id) {
                queue.extend(kids);
            }
        }
        for id in &subtree {
            ToolCall::filter_by_message_id(*id)
                .delete()
                .exec(&mut self.db)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
            MessageEmbedding::filter_by_message_id(*id)
                .delete()
                .exec(&mut self.db)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
            Message::delete_by_id(&mut self.db, *id)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
        }
        Ok(subtree)
    }

    pub async fn delete_session(&mut self, id: uuid::Uuid) -> Result<()> {
        Session::filter_by_id(id)
            .delete()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        toasty::sql::statement("DELETE FROM session_locks WHERE session_id = ?1")
            .bind_typed(id.as_bytes().to_vec(), db::Type::Blob)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn search_messages(&mut self, query: &str, limit: u64) -> Result<Vec<SearchHit>> {
        let rows = toasty::sql::query(
            "SELECT m.id, m.session_id, m.seq, m.role, m.content, s.title, \
             fts_score(m.content, ?1) AS score \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE fts_match(m.content, ?1) \
             ORDER BY score DESC \
             LIMIT ?2",
        )
        .bind(query)
        .bind(limit as i64)
        .column_types([
            Type::I64,
            Type::Bytes,
            Type::I64,
            Type::String,
            Type::String,
            Type::String,
            Type::F64,
        ])
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Search(e.to_string()))?;

        let mut hits = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(fields) = row else {
                continue;
            };
            let get_u64 = |i: usize| -> u64 {
                match &fields[i] {
                    toasty::stmt::Value::I64(v) => *v as u64,
                    toasty::stmt::Value::U64(v) => *v,
                    _ => 0,
                }
            };
            let get_str = |i: usize| -> String {
                match &fields[i] {
                    toasty::stmt::Value::String(s) => s.clone(),
                    _ => String::new(),
                }
            };
            let get_uuid = |i: usize| -> uuid::Uuid {
                match &fields[i] {
                    toasty::stmt::Value::Bytes(b) => uuid::Uuid::from_slice(b).unwrap_or_default(),
                    _ => uuid::Uuid::nil(),
                }
            };
            let get_f64 = |i: usize| -> f64 {
                match &fields[i] {
                    toasty::stmt::Value::F64(v) => *v,
                    toasty::stmt::Value::I64(v) => *v as f64,
                    _ => 0.0,
                }
            };
            hits.push(SearchHit {
                message_id: get_u64(0),
                session_id: get_uuid(1),
                seq: get_u64(2),
                role: MsgRole::from_str_loose(&get_str(3)),
                content: get_str(4),
                session_title: get_str(5),
                score: get_f64(6),
                source: SearchSource::Fts,
            });
        }
        Ok(hits)
    }

    pub async fn upsert_embedding(
        &mut self,
        message_id: u64,
        session_id: uuid::Uuid,
        seq: u64,
        content: &str,
        vec: Vec<u8>,
    ) -> Result<()> {
        let existing = MessageEmbedding::filter_by_message_id(message_id)
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        if let Some(emb) = existing {
            let update = MessageEmbedding::update_by_id(emb.id)
                .content(content.to_string())
                .vec(vec);
            update
                .exec(&mut self.db)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
            return Ok(());
        }
        toasty::create!(MessageEmbedding {
            message_id,
            session_id,
            seq,
            content: content.to_string(),
            vec,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn messages_missing_embeddings(
        &mut self,
        limit: u64,
    ) -> Result<Vec<EmbeddableMessage>> {
        let rows = toasty::sql::query(
            "SELECT m.id, m.session_id, m.seq, m.content \
             FROM messages m LEFT JOIN message_embeddings e ON e.message_id = m.id \
             WHERE e.id IS NULL \
             ORDER BY m.id ASC \
             LIMIT ?1",
        )
        .bind(limit as i64)
        .column_types([Type::I64, Type::Bytes, Type::I64, Type::String])
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Search(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(fields) = row else {
                continue;
            };
            let get_u64 = |i: usize| -> u64 {
                match &fields[i] {
                    toasty::stmt::Value::I64(v) => *v as u64,
                    toasty::stmt::Value::U64(v) => *v,
                    _ => 0,
                }
            };
            let get_uuid = |i: usize| -> uuid::Uuid {
                match &fields[i] {
                    toasty::stmt::Value::Bytes(b) => uuid::Uuid::from_slice(b).unwrap_or_default(),
                    _ => uuid::Uuid::nil(),
                }
            };
            let get_str = |i: usize| -> String {
                match &fields[i] {
                    toasty::stmt::Value::String(s) => s.clone(),
                    _ => String::new(),
                }
            };
            out.push(EmbeddableMessage {
                id: get_u64(0),
                session_id: get_uuid(1),
                seq: get_u64(2),
                content: get_str(3),
            });
        }
        Ok(out)
    }

    pub async fn semantic_search(&mut self, vec: Vec<f32>, limit: u64) -> Result<Vec<SearchHit>> {
        let vec_blob = f32_blob(&vec);
        let rows = toasty::sql::query(
            "SELECT m.id, m.session_id, m.seq, m.role, m.content, s.title, \
             vector_distance_cos(e.vec, ?1) AS d \
             FROM message_embeddings e \
             JOIN messages m ON m.id = e.message_id \
             JOIN sessions s ON s.id = m.session_id \
             ORDER BY d ASC \
             LIMIT ?2",
        )
        .bind_typed(toasty::stmt::Value::Bytes(vec_blob), db::Type::Blob)
        .bind(limit as i64)
        .column_types([
            Type::I64,
            Type::Bytes,
            Type::I64,
            Type::String,
            Type::String,
            Type::String,
            Type::F64,
        ])
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Search(e.to_string()))?;

        let mut hits = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(fields) = row else {
                continue;
            };
            let get_u64 = |i: usize| -> u64 {
                match &fields[i] {
                    toasty::stmt::Value::I64(v) => *v as u64,
                    toasty::stmt::Value::U64(v) => *v,
                    _ => 0,
                }
            };
            let get_str = |i: usize| -> String {
                match &fields[i] {
                    toasty::stmt::Value::String(s) => s.clone(),
                    _ => String::new(),
                }
            };
            let get_uuid = |i: usize| -> uuid::Uuid {
                match &fields[i] {
                    toasty::stmt::Value::Bytes(b) => uuid::Uuid::from_slice(b).unwrap_or_default(),
                    _ => uuid::Uuid::nil(),
                }
            };
            let get_f64 = |i: usize| -> f64 {
                match &fields[i] {
                    toasty::stmt::Value::F64(v) => *v,
                    toasty::stmt::Value::I64(v) => *v as f64,
                    _ => 0.0,
                }
            };
            hits.push(SearchHit {
                message_id: get_u64(0),
                session_id: get_uuid(1),
                seq: get_u64(2),
                role: MsgRole::from_str_loose(&get_str(3)),
                content: get_str(4),
                session_title: get_str(5),
                score: get_f64(6),
                source: SearchSource::Semantic,
            });
        }
        Ok(hits)
    }

    async fn messages_for_session(&mut self, id: uuid::Uuid) -> Result<Vec<StoredMessage>> {
        let messages = Message::filter_by_session_id(id)
            .order_by(Message::fields().seq().asc())
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(messages.into_iter().map(StoredMessage::from).collect())
    }

    async fn message_count(&mut self, session_id: uuid::Uuid) -> Result<u64> {
        let count: u64 = Message::filter_by_session_id(session_id)
            .count()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(count)
    }

    async fn next_seq(&mut self, session_id: uuid::Uuid) -> Result<u64> {
        let last = Message::filter_by_session_id(session_id)
            .latest_by(Message::fields().seq())
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(last.map(|m| m.seq + 1).unwrap_or(0))
    }

    pub async fn last_turn(
        &mut self,
        session_id: uuid::Uuid,
    ) -> Result<Option<(StoredMessage, StoredMessage)>> {
        let messages = self.messages_for_session(session_id).await?;
        let mut i = messages.len();
        while i >= 2 {
            i -= 1;
            if messages[i].role == MsgRole::Assistant && messages[i - 1].role == MsgRole::User {
                return Ok(Some((messages[i - 1].clone(), messages[i].clone())));
            }
        }
        Ok(None)
    }

    pub async fn delete_message(&mut self, message_id: u64) -> Result<()> {
        Message::delete_by_id(&mut self.db, message_id)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn delete_tool_calls_for_message(&mut self, message_id: u64) -> Result<()> {
        ToolCall::filter_by_message_id(message_id)
            .delete()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    async fn touch_session(&mut self, id: uuid::Uuid) -> Result<()> {
        let now = jiff::Timestamp::now();
        let update = Session::update_by_id(id).updated_at(now);
        update
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    /// Persist a session's chat scroll position. A raw update bypasses the
    /// model's auto-timestamp, so `updated_at` is untouched and scrolling
    /// never reorders the session list.
    pub async fn set_scroll(&mut self, id: uuid::Uuid, scroll: StoredScroll) -> Result<()> {
        toasty::sql::statement(
            "UPDATE sessions SET scroll_sticky = ?1, scroll_turn = ?2, scroll_row = ?3 \
             WHERE id = ?4",
        )
        .bind_typed(scroll.sticky, db::Type::Boolean)
        .bind_typed(
            scroll.anchor.map(|(turn, _)| turn),
            db::Type::UnsignedInteger(8),
        )
        .bind_typed(
            scroll.anchor.map(|(_, row)| row),
            db::Type::UnsignedInteger(8),
        )
        .bind_typed(id.as_bytes().to_vec(), db::Type::Blob)
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    /// Rename a session. A raw update bypasses the model's auto-timestamp, so
    /// `updated_at` is untouched and renaming never reorders the session list.
    pub async fn set_title(&mut self, id: uuid::Uuid, title: &str) -> Result<()> {
        toasty::sql::statement("UPDATE sessions SET title = ?1 WHERE id = ?2")
            .bind_typed(title, db::Type::Text)
            .bind_typed(id.as_bytes().to_vec(), db::Type::Blob)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    /// Set the session's active scene. A raw update bypasses the model's
    /// auto-timestamp, so `updated_at` is untouched and a scene switch never
    /// reorders the session list.
    pub async fn set_scene(&mut self, id: uuid::Uuid, scene: Option<&str>) -> Result<()> {
        toasty::sql::statement("UPDATE sessions SET scene = ?1 WHERE id = ?2")
            .bind_typed(scene, db::Type::Text)
            .bind_typed(id.as_bytes().to_vec(), db::Type::Blob)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn acquire_session_lock(
        &mut self,
        session_id: uuid::Uuid,
        now_ms: i64,
    ) -> Result<LockAcquire> {
        let Some(client_id) = self.client_id.clone() else {
            return Ok(LockAcquire::Acquired);
        };
        let cutoff = now_ms - SESSION_LOCK_TTL_MS;
        if let Some((holder, beat)) = self.lock_row(session_id).await? {
            if holder == client_id.as_ref() {
                self.touch_session_lock(session_id, now_ms).await?;
                return Ok(LockAcquire::Ours);
            }
            if beat >= cutoff {
                return Ok(LockAcquire::Held);
            }
            let taken = self
                .takeover_session_lock(session_id, &client_id, now_ms, cutoff)
                .await?;
            return Ok(if taken > 0 {
                LockAcquire::Acquired
            } else {
                LockAcquire::Held
            });
        }
        match toasty::sql::statement(
            "INSERT INTO session_locks (session_id, client_id, beat) VALUES (?1, ?2, ?3)",
        )
        .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
        .bind_typed(client_id.to_string(), db::Type::Text)
        .bind(now_ms)
        .exec(&mut self.db)
        .await
        {
            Ok(_) => Ok(LockAcquire::Acquired),
            Err(_) => {
                let taken = self
                    .takeover_session_lock(session_id, &client_id, now_ms, cutoff)
                    .await?;
                Ok(if taken > 0 {
                    LockAcquire::Acquired
                } else {
                    LockAcquire::Held
                })
            }
        }
    }

    pub async fn touch_session_lock(
        &mut self,
        session_id: uuid::Uuid,
        now_ms: i64,
    ) -> Result<bool> {
        let Some(client_id) = self.client_id.clone() else {
            return Ok(true);
        };
        let updated = toasty::sql::statement(
            "UPDATE session_locks SET beat = ?1 WHERE session_id = ?2 AND client_id = ?3",
        )
        .bind(now_ms)
        .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
        .bind_typed(client_id.to_string(), db::Type::Text)
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(updated > 0)
    }

    pub async fn release_session_lock(&mut self, session_id: uuid::Uuid) -> Result<()> {
        let Some(client_id) = self.client_id.clone() else {
            return Ok(());
        };
        toasty::sql::statement(
            "DELETE FROM session_locks WHERE session_id = ?1 AND client_id = ?2",
        )
        .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
        .bind_typed(client_id.to_string(), db::Type::Text)
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn locked_by_other(&mut self, session_id: uuid::Uuid, now_ms: i64) -> Result<bool> {
        let cutoff = now_ms - SESSION_LOCK_TTL_MS;
        Ok(match self.lock_row(session_id).await? {
            Some((holder, beat)) => {
                beat >= cutoff && Some(holder.as_str()) != self.client_id.as_deref()
            }
            None => false,
        })
    }

    async fn ensure_session_locks(&mut self) -> Result<()> {
        toasty::sql::statement(
            "CREATE TABLE IF NOT EXISTS session_locks ( \
                 session_id BLOB PRIMARY KEY, \
                 client_id TEXT NOT NULL, \
                 beat INTEGER NOT NULL)",
        )
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    async fn lock_row(&mut self, session_id: uuid::Uuid) -> Result<Option<(String, i64)>> {
        let rows =
            toasty::sql::query("SELECT client_id, beat FROM session_locks WHERE session_id = ?1")
                .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
                .column_types([Type::String, Type::I64])
                .exec(&mut self.db)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(rows.into_iter().next().and_then(|row| match row {
            toasty::stmt::Value::Record(fields) => Some((
                match &fields[0] {
                    toasty::stmt::Value::String(s) => s.clone(),
                    _ => String::new(),
                },
                match &fields[1] {
                    toasty::stmt::Value::I64(v) => *v,
                    _ => 0,
                },
            )),
            _ => None,
        }))
    }

    async fn takeover_session_lock(
        &mut self,
        session_id: uuid::Uuid,
        client_id: &str,
        now_ms: i64,
        cutoff: i64,
    ) -> Result<u64> {
        toasty::sql::statement(
            "UPDATE session_locks SET client_id = ?2, beat = ?3 \
             WHERE session_id = ?1 AND client_id != ?2 AND beat < ?4",
        )
        .bind_typed(session_id.as_bytes().to_vec(), db::Type::Blob)
        .bind_typed(client_id.to_string(), db::Type::Text)
        .bind(now_ms)
        .bind(cutoff)
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))
    }

    async fn locked_session_ids(&mut self, now_ms: i64) -> Result<HashSet<uuid::Uuid>> {
        let cutoff = now_ms - SESSION_LOCK_TTL_MS;
        let client_id = self.client_id.clone();
        let query = match &client_id {
            Some(client) => toasty::sql::query(
                "SELECT session_id FROM session_locks WHERE beat >= ?1 AND client_id != ?2",
            )
            .bind(cutoff)
            .bind_typed(client.to_string(), db::Type::Text),
            None => toasty::sql::query("SELECT session_id FROM session_locks WHERE beat >= ?1")
                .bind(cutoff),
        };
        let rows = query
            .column_types([Type::Bytes])
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        let mut out = HashSet::new();
        for row in rows {
            if let toasty::stmt::Value::Record(fields) = row
                && let toasty::stmt::Value::Bytes(b) = &fields[0]
                && let Ok(id) = uuid::Uuid::from_slice(b)
            {
                out.insert(id);
            }
        }
        Ok(out)
    }
}

fn scroll_of(session: &Session) -> StoredScroll {
    StoredScroll {
        sticky: session.scroll_sticky,
        anchor: session.scroll_turn.zip(session.scroll_row),
    }
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn f32_blob(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}
