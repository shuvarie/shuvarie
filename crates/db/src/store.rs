use std::path::{Path, PathBuf};

use shuvarie_llm::Role;
use shuvarie_llm::TokenUsage;
use toasty::db::Driver;
use toasty::schema::db;
use toasty::stmt::{List, Query, Type};

use crate::error::{DbError, Result};
use crate::model::{Message, MessageEmbedding, MsgRole, Session, ToolCall, UndoLog};

static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: uuid::Uuid,
    pub title: String,
    pub message_count: u64,
    pub updated_at_epoch_ms: i64,
}

#[derive(Debug, Clone)]
pub struct StoredSession {
    pub id: uuid::Uuid,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub messages: Vec<StoredMessage>,
    pub tool_calls: Vec<StoredToolCall>,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: u64,
    pub role: MsgRole,
    pub content: String,
    pub reasoning: String,
    pub interrupted: bool,
    pub seq: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub summary: bool,
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
    pub ok: bool,
    pub worker: Option<String>,
    pub file_change_json: String,
    pub original_content: Option<String>,
    pub new_content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub turn_seq: u64,
    pub user_content: String,
    pub assistant_content: String,
    pub reasoning: String,
    pub usage: TokenUsage,
    pub cost: f64,
    pub tool_calls: Vec<StoredToolCall>,
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
            role: m.role,
            content: m.content,
            reasoning: m.reasoning,
            interrupted: m.interrupted,
            seq: m.seq,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            total_tokens: m.total_tokens,
            cached_input_tokens: m.cached_input_tokens,
            reasoning_tokens: m.reasoning_tokens,
            cost: m.cost,
            summary: m.summary,
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
            ok: t.ok,
            worker: t.worker,
            file_change_json: t.file_change_json,
            original_content: t.original_content,
            new_content: t.new_content,
        }
    }
}

#[derive(Clone)]
pub struct Store {
    db: toasty::Db,
}

impl Store {
    pub fn default_path() -> PathBuf {
        PathBuf::from(".shuvarie").join("data.db")
    }

    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DbError::Open(format!("create dir {}: {e}", parent.display())))?;
        }
        let driver = toasty_driver_turso::Turso::file(path).experimental_index_method(true);
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
                ToolCall,
                UndoLog
            ))
            .build(driver)
            .await
            .map_err(|e| DbError::Open(e.to_string()))?;
        MIGRATIONS
            .apply(&db)
            .await
            .map_err(|e| DbError::Migration(e.to_string()))?;
        Ok(Self { db })
    }

    pub async fn list_sessions(&mut self) -> Result<Vec<SessionSummary>> {
        let sessions = Query::<List<Session>>::all()
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
        Ok(StoredSession {
            id: session.id,
            title: session.title,
            provider: session.provider,
            model: session.model,
            messages,
            tool_calls,
        })
    }

    pub async fn most_recent_session(&mut self) -> Result<Option<StoredSession>> {
        let latest = Query::<List<Session>>::all()
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
        Ok(Some(StoredSession {
            id: session.id,
            title: session.title,
            provider: session.provider,
            model: session.model,
            messages,
            tool_calls,
        }))
    }

    pub async fn create_session(
        &mut self,
        title: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<uuid::Uuid> {
        let session = toasty::create!(Session {
            title: title.to_string(),
            provider: provider.map(|p| p.to_string()),
            model: model.map(|m| m.to_string()),
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(session.id)
    }

    pub async fn append_message(
        &mut self,
        session_id: uuid::Uuid,
        role: Role,
        content: &str,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            seq,
            role: MsgRole::from(role),
            content: content.to_string(),
            reasoning: String::new(),
            interrupted: false,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: false,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    pub async fn append_assistant_message(
        &mut self,
        session_id: uuid::Uuid,
        content: &str,
        reasoning: &str,
        interrupted: bool,
        usage: TokenUsage,
        cost: f64,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            seq,
            role: MsgRole::Assistant,
            content: content.to_string(),
            reasoning: reasoning.to_string(),
            interrupted,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost,
            summary: false,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    pub async fn update_message(
        &mut self,
        message_id: u64,
        content: &str,
        reasoning: &str,
        interrupted: bool,
        usage: TokenUsage,
        cost: f64,
    ) -> Result<()> {
        Message::update_by_id(message_id)
            .content(content.to_string())
            .reasoning(reasoning.to_string())
            .interrupted(interrupted)
            .input_tokens(usage.input_tokens)
            .output_tokens(usage.output_tokens)
            .total_tokens(usage.total_tokens)
            .cached_input_tokens(usage.cached_input_tokens)
            .reasoning_tokens(usage.reasoning_tokens)
            .cost(cost)
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn append_summary(
        &mut self,
        session_id: uuid::Uuid,
        content: &str,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            seq,
            role: MsgRole::Assistant,
            content: content.to_string(),
            reasoning: String::new(),
            interrupted: false,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: true,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
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
        ok: bool,
        worker: Option<&str>,
        file_change_json: &str,
        original_content: Option<&str>,
        new_content: Option<&str>,
    ) -> Result<u64> {
        let tc = toasty::create!(ToolCall {
            session_id,
            message_id,
            seq,
            name: name.to_string(),
            args_json: args_json.to_string(),
            output: output.to_string(),
            ok,
            worker: worker.map(|s| s.to_string()),
            file_change_json: file_change_json.to_string(),
            original_content: original_content.map(|s| s.to_string()),
            new_content: new_content.map(|s| s.to_string()),
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

    pub async fn truncate_undo_log(&mut self, session_id: uuid::Uuid) -> Result<()> {
        UndoLog::filter_by_session_id(session_id)
            .delete()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn append_undo_log(
        &mut self,
        session_id: uuid::Uuid,
        entry: &UndoEntry,
    ) -> Result<()> {
        let usage_json = serde_json::to_string(&entry.usage).unwrap_or_default();
        let tool_calls_json = serialize_tool_calls(&entry.tool_calls);
        let file_changes_json = serialize_file_changes(&entry.tool_calls);
        toasty::create!(UndoLog {
            session_id,
            turn_seq: entry.turn_seq,
            user_content: entry.user_content.clone(),
            assistant_content: entry.assistant_content.clone(),
            reasoning: entry.reasoning.clone(),
            usage_json,
            tool_calls_json,
            file_changes_json,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
    }

    pub async fn last_undo_log(&mut self, session_id: uuid::Uuid) -> Result<Option<UndoEntry>> {
        let row = UndoLog::filter_by_session_id(session_id)
            .latest_by(UndoLog::fields().id())
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(UndoEntry {
            turn_seq: row.turn_seq,
            user_content: row.user_content,
            assistant_content: row.assistant_content,
            reasoning: row.reasoning,
            usage: serde_json::from_str(&row.usage_json).unwrap_or_default(),
            cost: 0.0,
            tool_calls: deserialize_tool_calls(&row.tool_calls_json),
        }))
    }

    pub async fn pop_undo_log(&mut self, session_id: uuid::Uuid) -> Result<Option<UndoEntry>> {
        let entry = self.last_undo_log(session_id).await?;
        if entry.is_some() {
            let row = UndoLog::filter_by_session_id(session_id)
                .latest_by(UndoLog::fields().id())
                .first()
                .exec(&mut self.db)
                .await
                .map_err(|e| DbError::Query(e.to_string()))?;
            if let Some(row) = row {
                UndoLog::delete_by_id(&mut self.db, row.id)
                    .await
                    .map_err(|e| DbError::Query(e.to_string()))?;
            }
        }
        Ok(entry)
    }

    pub async fn delete_session(&mut self, id: uuid::Uuid) -> Result<()> {
        Session::filter_by_id(id)
            .delete()
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
}

fn f32_blob(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SerializableToolCall {
    name: String,
    args_json: String,
    output: String,
    ok: bool,
    worker: Option<String>,
    file_change_json: String,
    original_content: Option<String>,
    new_content: Option<String>,
}

impl From<&StoredToolCall> for SerializableToolCall {
    fn from(t: &StoredToolCall) -> Self {
        Self {
            name: t.name.clone(),
            args_json: t.args_json.clone(),
            output: t.output.clone(),
            ok: t.ok,
            worker: t.worker.clone(),
            file_change_json: t.file_change_json.clone(),
            original_content: t.original_content.clone(),
            new_content: t.new_content.clone(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SerializableFileChange {
    path: String,
    original_content: Option<String>,
    new_content: Option<String>,
}

fn serialize_tool_calls(tool_calls: &[StoredToolCall]) -> String {
    let entries: Vec<SerializableToolCall> =
        tool_calls.iter().map(SerializableToolCall::from).collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

fn serialize_file_changes(tool_calls: &[StoredToolCall]) -> String {
    let mut entries = Vec::new();
    for tc in tool_calls {
        if tc.file_change_json.is_empty() {
            continue;
        }
        entries.push(SerializableFileChange {
            path: path_from_file_change_json(&tc.file_change_json),
            original_content: tc.original_content.clone(),
            new_content: tc.new_content.clone(),
        });
    }
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

fn path_from_file_change_json(json: &str) -> String {
    #[derive(serde::Deserialize)]
    struct FcPath {
        path: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", content = "path")]
    enum FcEnvelope {
        Edit { path: String },
        Write { path: String },
    }
    if let Ok(env) = serde_json::from_str::<FcEnvelope>(json) {
        return match env {
            FcEnvelope::Edit { path } | FcEnvelope::Write { path } => path,
        };
    }
    if let Ok(p) = serde_json::from_str::<FcPath>(json) {
        return p.path;
    }
    String::new()
}

fn deserialize_tool_calls(json: &str) -> Vec<StoredToolCall> {
    let Ok(entries) = serde_json::from_str::<Vec<SerializableToolCall>>(json) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .enumerate()
        .map(|(seq, e)| StoredToolCall {
            id: 0,
            message_id: 0,
            session_id: uuid::Uuid::nil(),
            seq: seq as u64,
            name: e.name,
            args_json: e.args_json,
            output: e.output,
            ok: e.ok,
            worker: e.worker,
            file_change_json: e.file_change_json,
            original_content: e.original_content,
            new_content: e.new_content,
        })
        .collect()
}
