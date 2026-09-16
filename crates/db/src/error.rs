use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("failed to open database: {0}")]
    Open(String),
    #[error("failed to apply migrations: {0}")]
    Migration(String),
    #[error("database query failed: {0}")]
    Query(String),
    #[error("search query failed: {0}")]
    Search(String),
    #[error("session file: {0}")]
    SessionFile(String),
    #[error("session {id} not found")]
    NotFound { id: uuid::Uuid },
}

pub type Result<T> = std::result::Result<T, DbError>;
