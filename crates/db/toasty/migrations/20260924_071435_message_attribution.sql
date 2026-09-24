ALTER TABLE "messages" ADD COLUMN "model_code" TEXT;
-- `sessions.scene` already exists (added to 0000_init.sql without a snapshot
-- refresh, so a naive diff against 0000 would emit a duplicate ADD COLUMN).
-- #[toasty::breakpoint]
ALTER TABLE "messages" ADD COLUMN "scene" TEXT;