CREATE TABLE "message_embeddings" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "message_id" INTEGER NOT NULL,
    "session_id" BLOB NOT NULL,
    "seq" INTEGER NOT NULL,
    "content" TEXT NOT NULL,
    "vec" BLOB NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_message_embeddings_by_message_id" ON "message_embeddings" ("message_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_message_embeddings_by_session_id" ON "message_embeddings" ("session_id");
-- #[toasty::breakpoint]
CREATE TABLE "messages" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "session_id" BLOB NOT NULL,
    "parent_id" INTEGER,
    "seq" INTEGER NOT NULL,
    "role" TEXT NOT NULL CHECK ("role" IN ('system', 'user', 'assistant')),
    "content" TEXT NOT NULL,
    "reasoning" TEXT NOT NULL,
    "text_segments" TEXT NOT NULL,
    "interrupted" BOOLEAN NOT NULL,
    "input_tokens" INTEGER NOT NULL,
    "output_tokens" INTEGER NOT NULL,
    "total_tokens" INTEGER NOT NULL,
    "cached_input_tokens" INTEGER NOT NULL,
    "reasoning_tokens" INTEGER NOT NULL,
    "cost" REAL NOT NULL,
    "summary" BOOLEAN NOT NULL,
    "request_json" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_messages_by_session_id" ON "messages" ("session_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_messages_by_parent_id" ON "messages" ("parent_id");
-- #[toasty::breakpoint]
CREATE TABLE "tool_calls" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "session_id" BLOB NOT NULL,
    "message_id" INTEGER NOT NULL,
    "seq" INTEGER NOT NULL,
    "name" TEXT NOT NULL,
    "args_json" TEXT NOT NULL,
    "output" TEXT NOT NULL,
    "ok" BOOLEAN NOT NULL,
    "killed" BOOLEAN NOT NULL,
    "worker" TEXT,
    "file_change_json" TEXT NOT NULL,
    "original_content" TEXT,
    "new_content" TEXT,
    "stderr" TEXT NOT NULL,
    "duration_ms" INTEGER NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_tool_calls_by_session_id" ON "tool_calls" ("session_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_tool_calls_by_message_id" ON "tool_calls" ("message_id");
-- #[toasty::breakpoint]
CREATE TABLE "sessions" (
    "id" BLOB NOT NULL,
    "title" TEXT NOT NULL,
    "provider" TEXT,
    "model" TEXT,
    "scene" TEXT,
    "session_type" TEXT NOT NULL CHECK ("session_type" IN ('main', 'worker')),
    "parent_id" BLOB,
    "leaf_id" INTEGER,
    "created_at" TEXT NOT NULL,
    "updated_at" TEXT NOT NULL,
    "scroll_sticky" BOOLEAN NOT NULL,
    "scroll_turn" INTEGER,
    "scroll_row" INTEGER,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_sessions_by_parent_id" ON "sessions" ("parent_id");
