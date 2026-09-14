use std::path::Path;

use serde_json::{Value, json};
use shuvarie_llm::{FileChange, Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::lsp_manager::SharedManager;
use crate::permissions::{Access, PathKind, resolve_write};

use super::{FileLocks, ReadCache, arg_value};

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

pub(crate) struct WriteFile {
    read_cache: ReadCache,
    lsp: Option<SharedManager>,
    locks: FileLocks,
    access: Access,
}

impl WriteFile {
    pub(crate) fn new(
        read_cache: ReadCache,
        lsp: Option<SharedManager>,
        locks: FileLocks,
        access: Access,
    ) -> Self {
        Self {
            read_cache,
            lsp,
            locks,
            access,
        }
    }
}

impl Tool for WriteFile {
    const NAME: &'static str = "write_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Create or overwrite a file in the working directory, creating parent directories as needed. \
         Passing mode=\"create\" fails when the file already exists; mode=\"overwrite\" requires reading \
         the file with read_file first. When mode is omitted, new files are created and existing files \
         are overwritten (also requiring a prior read)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to write" },
                "content": { "type": "string", "description": "Full new contents of the file" },
                "mode": { "type": "string", "enum": ["create", "overwrite"], "description": "Explicit write mode: 'create' fails if the file exists; 'overwrite' replaces an existing file (must be read first). Omit to auto-detect" }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let read_cache = self.read_cache.clone();
        let lsp = self.lsp.clone();
        let locks = self.locks.clone();
        let access = self.access.clone();
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let content = arg_value(&args, "content")?;
            let mode = args.get("mode").and_then(Value::as_str);
            match mode {
                Some("create") | Some("overwrite") | None => {}
                Some(other) => return Err(format!("invalid mode '{other}' (expected 'create' or 'overwrite')")),
            }
            let abs = resolve_write(&path)?;
            access.authorize_path(PathKind::Write, &abs, &path).await?;
            let _file_lock = locks.lock(&abs).await;
            let exists = abs.exists();
            match mode {
                Some("create") if exists => {
                    return Err(format!(
                        "'{path}' already exists; use mode 'overwrite' (after reading it) to replace it"
                    ));
                }
                Some("overwrite") | None if exists && !read_cache.was_read(&path) => {
                    return Err(format!(
                        "'{path}' exists but was not read this turn; read it with read_file before overwriting, or pass mode 'create' for a new file"
                    ));
                }
                _ => {}
            }
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
            }
            let original = tokio::fs::read(&abs).await.ok();
            let original_text = original
                .as_deref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
            let had_bom = original.as_deref().is_some_and(|b| b.starts_with(UTF8_BOM));
            let content_bare = content.trim_start_matches('\u{FEFF}');
            let content = if had_bom || content_bare.len() != content.len() {
                format!("\u{FEFF}{content_bare}")
            } else {
                content_bare.to_string()
            };
            let tmp_path = abs.with_extension(format!(
                "{}tmp",
                abs.extension()
                    .map(|e| format!("{e}.", e = e.to_string_lossy()))
                    .unwrap_or_default()
            ));
            tokio::fs::write(&tmp_path, &content)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            if let Err(e) = tokio::fs::rename(&tmp_path, &abs).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(format!("write {path}: {e}"));
            }
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_change(Path::new(&path), &content)
                    .await;
            }
            let summary = format!("wrote {} bytes to {path}", content.len());
            ctx.insert_result(FileChange::Write {
                path,
                content,
                original: original_text,
            });
            Ok(ToolOutput::text(summary))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};
    use crate::tools::read_file::ReadFile;

    fn write_file_tool(read_cache: ReadCache) -> WriteFile {
        WriteFile::new(
            read_cache,
            None,
            FileLocks::new(),
            crate::test_util::access(),
        )
    }

    #[tokio::test]
    async fn write_creates_parents() {
        let (dir, _guard) = tempdir();
        let mut ctx = new_ctx();
        let _out = write_file_tool(ReadCache::new())
            .call(
                &mut ctx,
                json!({ "path": "sub/deep/f.txt", "content": "hello" }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("sub/deep/f.txt").unwrap(), "hello");
        assert!(matches!(
            ctx.result::<FileChange>(),
            Some(FileChange::Write { path, .. }) if path == "sub/deep/f.txt"
        ));
        drop(dir);
    }

    #[tokio::test]
    async fn write_denied_by_rule_errors() {
        let (dir, _guard) = tempdir();
        let tool = WriteFile::new(
            ReadCache::new(),
            None,
            FileLocks::new(),
            crate::test_util::access_for_config(&shuvarie_config::PermissionsConfig {
                default: Some(shuvarie_config::Verb::Deny),
                paths: shuvarie_config::RuleSet::default(),
                ..shuvarie_config::PermissionsConfig::builtin()
            }),
        );
        let err = tool
            .call(&mut new_ctx(), json!({ "path": "new.txt", "content": "x" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("permission denied"),
            "{}",
            err.to_string()
        );
        assert!(!std::fs::exists("new.txt").unwrap());
        drop(dir);
    }

    #[tokio::test]
    async fn write_create_mode_fails_on_existing() {
        let (dir, _guard) = tempdir();
        std::fs::write("exists.txt", "old").unwrap();
        let cache = ReadCache::new();
        let err = write_file_tool(cache)
            .call(
                &mut new_ctx(),
                json!({ "path": "exists.txt", "content": "new", "mode": "create" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "{}",
            err.to_string()
        );
        assert_eq!(std::fs::read_to_string("exists.txt").unwrap(), "old");
        drop(dir);
    }

    #[tokio::test]
    async fn write_overwrite_requires_read_first() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "old").unwrap();
        let cache = ReadCache::new();
        let tool = write_file_tool(cache.clone());
        let err = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "f.txt", "content": "new", "mode": "overwrite" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("was not read"),
            "{}",
            err.to_string()
        );
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "old");

        let reader = ReadFile::new(cache, 0, 0, crate::test_util::access());
        reader
            .call(&mut new_ctx(), json!({ "path": "f.txt" }))
            .await
            .unwrap();
        tool.call(
            &mut new_ctx(),
            json!({ "path": "f.txt", "content": "new", "mode": "overwrite" }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "new");
        drop(dir);
    }

    #[tokio::test]
    async fn write_atomic_via_temp_rename() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "v1").unwrap();
        let cache = ReadCache::new();
        let reader = ReadFile::new(cache.clone(), 0, 0, crate::test_util::access());
        reader
            .call(&mut new_ctx(), json!({ "path": "f.txt" }))
            .await
            .unwrap();
        write_file_tool(cache)
            .call(&mut new_ctx(), json!({ "path": "f.txt", "content": "v2" }))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "v2");
        let leftovers: Vec<_> = std::fs::read_dir(".")
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                name.contains("tmp").then_some(name)
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn write_preserves_bom_and_strips_duplicate() {
        let (dir, _guard) = tempdir();
        std::fs::write("bom.txt", "\u{FEFF}original").unwrap();
        let cache = ReadCache::new();
        let reader = ReadFile::new(cache.clone(), 0, 0, crate::test_util::access());
        reader
            .call(&mut new_ctx(), json!({ "path": "bom.txt" }))
            .await
            .unwrap();
        write_file_tool(cache)
            .call(
                &mut new_ctx(),
                json!({ "path": "bom.txt", "content": "\u{FEFF}replaced" }),
            )
            .await
            .unwrap();
        let bytes = std::fs::read("bom.txt").unwrap();
        assert!(bytes.starts_with(UTF8_BOM));
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            "\u{FEFF}replaced"
        );
        assert_eq!(&bytes[3..], "replaced".as_bytes());
        drop(dir);
    }

    #[tokio::test]
    async fn write_rejects_invalid_mode() {
        let (dir, _guard) = tempdir();
        let err = write_file_tool(ReadCache::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "f.txt", "content": "x", "mode": "append" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid mode"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }
}
