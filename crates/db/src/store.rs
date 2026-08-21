use std::path::{Path, PathBuf};

use shuvarie_catalog::TokenUsage;
use shuvarie_llm::Role;
use toasty::db::Driver;
use toasty::schema::db;
use toasty::stmt::{List, Query, Type};

use crate::error::{DbError, Result};
use crate::model::{Message, MessageEmbedding, MsgRole, Session};

static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: u64,
    pub title: String,
    pub message_count: u64,
    pub updated_at_epoch_ms: i64,
}

#[derive(Debug, Clone)]
pub struct StoredSession {
    pub id: u64,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub messages: Vec<StoredMessage>,
}

#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub id: u64,
    pub role: MsgRole,
    pub content: String,
    pub seq: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    Fts,
    Semantic,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub message_id: u64,
    pub session_id: u64,
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
    pub session_id: u64,
    pub seq: u64,
    pub content: String,
}

impl From<Message> for StoredMessage {
    fn from(m: Message) -> Self {
        Self {
            id: m.id,
            role: m.role,
            content: m.content,
            seq: m.seq,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            total_tokens: m.total_tokens,
            cached_input_tokens: m.cached_input_tokens,
            reasoning_tokens: m.reasoning_tokens,
            cost: m.cost,
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
            .models(toasty::models!(Session, Message, MessageEmbedding))
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

    pub async fn load_session(&mut self, id: u64) -> Result<StoredSession> {
        let session = Session::filter_by_id(id)
            .first()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?
            .ok_or(DbError::NotFound { id })?;
        let messages = self.messages_for_session(id).await?;
        Ok(StoredSession {
            id: session.id,
            title: session.title,
            provider: session.provider,
            model: session.model,
            messages,
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
        Ok(Some(StoredSession {
            id: session.id,
            title: session.title,
            provider: session.provider,
            model: session.model,
            messages,
        }))
    }

    pub async fn create_session(
        &mut self,
        title: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<u64> {
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
        session_id: u64,
        role: Role,
        content: &str,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            seq,
            role: MsgRole::from(role),
            content: content.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    pub async fn append_assistant_message(
        &mut self,
        session_id: u64,
        content: &str,
        usage: TokenUsage,
        cost: f64,
    ) -> Result<StoredMessage> {
        let seq = self.next_seq(session_id).await?;
        let msg = toasty::create!(Message {
            session_id,
            seq,
            role: MsgRole::Assistant,
            content: content.to_string(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost,
        })
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Query(e.to_string()))?;
        self.touch_session(session_id).await?;
        Ok(StoredMessage::from(msg))
    }

    pub async fn delete_session(&mut self, id: u64) -> Result<()> {
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
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::String,
            toasty::stmt::Type::F64,
        ])
        .exec(&mut self.db)
        .await
        .map_err(|e| DbError::Search(e.to_string()))?;

        let mut hits = Vec::with_capacity(rows.len());
        for row in rows {
            let toasty::stmt::Value::Record(fields) = row else {
                continue;
            };
            let get_str = |i: usize| -> String {
                match &fields[i] {
                    toasty::stmt::Value::String(s) => s.clone(),
                    _ => String::new(),
                }
            };
            let get_u64 = |i: usize| -> u64 {
                match &fields[i] {
                    toasty::stmt::Value::I64(v) => *v as u64,
                    toasty::stmt::Value::U64(v) => *v,
                    _ => 0,
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
                session_id: get_u64(1),
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
        session_id: u64,
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
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::String,
        ])
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
            let get_str = |i: usize| -> String {
                match &fields[i] {
                    toasty::stmt::Value::String(s) => s.clone(),
                    _ => String::new(),
                }
            };
            out.push(EmbeddableMessage {
                id: get_u64(0),
                session_id: get_u64(1),
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
            Type::I64,
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
            let get_f64 = |i: usize| -> f64 {
                match &fields[i] {
                    toasty::stmt::Value::F64(v) => *v,
                    toasty::stmt::Value::I64(v) => *v as f64,
                    _ => 0.0,
                }
            };
            hits.push(SearchHit {
                message_id: get_u64(0),
                session_id: get_u64(1),
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

    async fn messages_for_session(&mut self, id: u64) -> Result<Vec<StoredMessage>> {
        let messages = Message::filter_by_session_id(id)
            .order_by(Message::fields().seq().asc())
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(messages.into_iter().map(StoredMessage::from).collect())
    }

    async fn message_count(&mut self, session_id: u64) -> Result<u64> {
        let count: u64 = Message::filter_by_session_id(session_id)
            .count()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(count)
    }

    async fn next_seq(&mut self, session_id: u64) -> Result<u64> {
        let count = self.message_count(session_id).await?;
        Ok(count)
    }

    async fn touch_session(&mut self, id: u64) -> Result<()> {
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
