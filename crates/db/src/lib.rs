pub mod error;
pub mod model;
pub mod store;

pub use error::{DbError, Result};
pub use model::{Message, MessageEmbedding, MsgRole, Session, ToolCall, UndoLog};
pub use store::{
    EmbeddableMessage, SearchHit, SearchSource, SessionSummary, Store, StoredMessage,
    StoredSession, StoredToolCall, UndoEntry,
};
