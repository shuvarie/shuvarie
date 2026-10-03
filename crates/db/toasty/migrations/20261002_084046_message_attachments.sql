CREATE TABLE "message_attachments" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "message_id" INTEGER NOT NULL,
    "session_id" BLOB NOT NULL,
    "seq" INTEGER NOT NULL,
    "kind" TEXT NOT NULL CHECK ("kind" IN ('image', 'document')),
    "name" TEXT NOT NULL,
    "media_type" TEXT NOT NULL,
    "size" INTEGER NOT NULL,
    "sha256" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_message_attachments_by_message_id" ON "message_attachments" ("message_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_message_attachments_by_session_id" ON "message_attachments" ("session_id");
-- #[toasty::breakpoint]
CREATE TABLE "attachment_blobs" (
    "sha256" TEXT NOT NULL,
    "size" INTEGER NOT NULL,
    "content" BLOB NOT NULL,
    PRIMARY KEY ("sha256")
);
