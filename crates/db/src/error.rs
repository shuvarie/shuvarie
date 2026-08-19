use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("failed to open database: {0}")]
    Open(String),
    #[error("failed to apply migrations: {0}")]
    Migration(String),
    #[error("database query failed: {0}")]
    Query(String),
    #[error("session {id} not found")]
    NotFound { id: u64 },
}

pub type Result<T> = std::result::Result<T, DbError>;
