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
    "input_tokens" INTEGER NOT NULL,
    "output_tokens" INTEGER NOT NULL,
    "total_tokens" INTEGER NOT NULL,
    "cached_input_tokens" INTEGER NOT NULL,
    "reasoning_tokens" INTEGER NOT NULL,
    "cost" REAL NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_messages_by_session_id" ON "messages" ("session_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_messages_fts" ON "messages" USING fts ("content");
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
