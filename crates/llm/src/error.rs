use thiserror::Error;

pub type Result<T> = std::result::Result<T, LlmError>;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("provider error: {0}")]
    Provider(String),

    #[error("model error: {0}")]
    Model(String),
}
