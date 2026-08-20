use std::path::{Path, PathBuf};

use shuvarie_catalog::TokenUsage;
use shuvarie_llm::Role;
use toasty::db::Driver;
use toasty::stmt::{List, Query};

use crate::error::{DbError, Result};
use crate::model::{Message, MsgRole, Session};

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

impl From<Message> for StoredMessage {
    fn from(m: Message) -> Self {
        Self {
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
        let driver = toasty_driver_turso::Turso::file(path);
        Self::open_with_driver(driver).await
    }

    pub async fn open_in_memory() -> Result<Self> {
        Self::open_with_driver(toasty_driver_turso::Turso::in_memory()).await
    }

    async fn open_with_driver(driver: impl Driver) -> Result<Self> {
        let db = toasty::Db::builder()
            .models(toasty::models!(Session))
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
    ) -> Result<()> {
        let seq = self.next_seq(session_id).await?;
        toasty::create!(Message {
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
        self.touch_session(session_id).await
    }

    pub async fn append_assistant_message(
        &mut self,
        session_id: u64,
        content: &str,
        usage: TokenUsage,
        cost: f64,
    ) -> Result<()> {
        let seq = self.next_seq(session_id).await?;
        toasty::create!(Message {
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
        self.touch_session(session_id).await
    }

    pub async fn delete_session(&mut self, id: u64) -> Result<()> {
        Session::filter_by_id(id)
            .delete()
            .exec(&mut self.db)
            .await
            .map_err(|e| DbError::Query(e.to_string()))?;
        Ok(())
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
