use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("config I/O error: {0}")]
    ConfigIo(#[from] std::io::Error),

    #[error("config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("config serialize error: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),
}
