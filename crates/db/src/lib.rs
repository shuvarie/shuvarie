pub mod error;
pub mod model;
pub mod session_file;
pub mod store;

pub use error::{DbError, Result};
pub use model::{
    Message, MessageEmbedding, MsgRole, ReasoningSegment, Session, SessionType, TextSegment,
    ToolCall, join_text_segments,
};
pub use session_file::{FileMessage, FileSession, FileToolCall, SessionFile};
pub use store::{
    Attribution, EMPTY_LEAF, EmbeddableMessage, LockAcquire, SESSION_LOCK_HEARTBEAT_MS,
    SESSION_LOCK_TTL_MS, SearchHit, SearchSource, SessionSummary, Store, StoredMessage,
    StoredScroll, StoredSession, StoredToolCall, WORKSPACE_DIR_NAME,
};
