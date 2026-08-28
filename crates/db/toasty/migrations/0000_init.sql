CREATE TABLE "sessions" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "title" TEXT NOT NULL,
    "provider" TEXT,
    "model" TEXT,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE TABLE "messages" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "session_id" INTEGER NOT NULL,
    "seq" INTEGER NOT NULL,
    "role" TEXT NOT NULL CHECK ("role" IN ('system', 'user', 'assistant')),
    "content" TEXT NOT NULL,
    "reasoning" TEXT NOT NULL DEFAULT '',
    "interrupted" INTEGER NOT NULL DEFAULT 0,
    "input_tokens" INTEGER NOT NULL,
    "output_tokens" INTEGER NOT NULL,
    "total_tokens" INTEGER NOT NULL,
    "cached_input_tokens" INTEGER NOT NULL,
    "reasoning_tokens" INTEGER NOT NULL,
    "cost" REAL NOT NULL,
    "summary" INTEGER NOT NULL DEFAULT 0
);
-- #[toasty::breakpoint]
CREATE INDEX "index_messages_by_session_id" ON "messages" ("session_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_messages_fts" ON "messages" USING fts ("content");
-- #[toasty::breakpoint]
CREATE TABLE "tool_calls" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "session_id" INTEGER NOT NULL,
    "message_id" INTEGER NOT NULL,
    "seq" INTEGER NOT NULL,
    "name" TEXT NOT NULL,
    "args_json" TEXT NOT NULL,
    "output" TEXT NOT NULL,
    "ok" INTEGER NOT NULL,
    "worker" TEXT,
    "file_change_json" TEXT NOT NULL DEFAULT '',
    "original_content" TEXT,
    "new_content" TEXT
);
-- #[toasty::breakpoint]
CREATE INDEX "index_tool_calls_by_session_id" ON "tool_calls" ("session_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_tool_calls_by_message_id" ON "tool_calls" ("message_id");
-- #[toasty::breakpoint]
CREATE TABLE "undo_log" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "session_id" INTEGER NOT NULL,
    "turn_seq" INTEGER NOT NULL,
    "user_content" TEXT NOT NULL,
    "assistant_content" TEXT NOT NULL,
    "reasoning" TEXT NOT NULL DEFAULT '',
    "usage_json" TEXT NOT NULL,
    "tool_calls_json" TEXT NOT NULL,
    "file_changes_json" TEXT NOT NULL DEFAULT ''
);
-- #[toasty::breakpoint]
CREATE INDEX "index_undo_log_by_session_id" ON "undo_log" ("session_id");
-- #[toasty::breakpoint]
CREATE TABLE "message_embeddings" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "message_id" INTEGER NOT NULL,
    "session_id" INTEGER NOT NULL,
    "seq" INTEGER NOT NULL,
    "content" TEXT NOT NULL,
    "vec" BLOB NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_message_embeddings_by_message_id" ON "message_embeddings" ("message_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_message_embeddings_by_session_id" ON "message_embeddings" ("session_id");
-- #[toasty::breakpoint]
CREATE TABLE "todos" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "session_id" INTEGER NOT NULL,
    "position" INTEGER NOT NULL,
    "content" TEXT NOT NULL,
    "status" TEXT NOT NULL,
    "priority" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_todos_by_session_id" ON "todos" ("session_id");
