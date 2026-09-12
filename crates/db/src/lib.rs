pub mod error;
pub mod model;
pub mod store;

pub use error::{DbError, Result};
pub use model::{
    Message, MessageEmbedding, MsgRole, ReasoningSegment, Session, SessionType, ToolCall, UndoLog,
};
pub use store::{
    EmbeddableMessage, SearchHit, SearchSource, SessionSummary, Store, StoredMessage, StoredScroll,
    StoredSession, StoredToolCall, UndoEntry, WORKSPACE_DIR_NAME,
};
