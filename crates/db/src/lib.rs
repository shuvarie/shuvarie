pub mod error;
pub mod model;
pub mod store;

pub use error::{DbError, Result};
pub use model::{
    Message, MessageEmbedding, MsgRole, ReasoningSegment, Session, SessionType, TextSegment,
    ToolCall, join_text_segments,
};
pub use store::{
    EmbeddableMessage, LockAcquire, SESSION_LOCK_HEARTBEAT_MS, SESSION_LOCK_TTL_MS, SearchHit,
    SearchSource, SessionSummary, Store, StoredMessage, StoredScroll, StoredSession,
    StoredToolCall, WORKSPACE_DIR_NAME,
};
