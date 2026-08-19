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
