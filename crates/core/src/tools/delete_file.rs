use serde_json::{Value, json};
use shuvarie_llm::{FileChange, Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind, resolve_write};

use super::FileLocks;

pub(crate) struct DeleteFile {
    locks: FileLocks,
    access: Access,
}

impl DeleteFile {
    pub(crate) fn new(locks: FileLocks, access: Access) -> Self {
        Self { locks, access }
    }
}

impl Tool for DeleteFile {
    const NAME: &'static str = "delete_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Permanently delete a file inside the working directory (files only, never \
         directories). For batch deletions combined with other edits prefer the \
         `apply_patch` tool."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to delete" }
            },
            "required": ["path"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let locks = self.locks.clone();
        let access = self.access.clone();
        let result: Result<ToolOutput, String> = async move {
            let path = super::arg_value(&args, "path")?;
            let abs = resolve_write(&path)?;
            access.authorize_path(PathKind::Write, &abs, &path).await?;
            let _file_lock = locks.lock(&abs).await;
            if abs.is_dir() {
                return Err(format!(
                    "'{path}' is a directory; delete_file only removes files"
                ));
            }
            let original = tokio::fs::read(&abs)
                .await
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            tokio::fs::remove_file(&abs)
                .await
                .map_err(|e| format!("delete {path}: {e}"))?;
            let summary = format!("deleted {path}");
            ctx.insert_result(FileChange::Delete { path, original });
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

    fn delete_file_tool() -> DeleteFile {
        DeleteFile::new(FileLocks::new(), crate::test_util::access())
    }

    #[tokio::test]
    async fn delete_removes_file_and_reports_original() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "gone soon").unwrap();
        let mut ctx = new_ctx();
        let out = delete_file_tool()
            .call(&mut ctx, json!({ "path": "f.txt" }))
            .await
            .unwrap();
        assert!(!std::fs::exists("f.txt").unwrap());
        assert!(out.as_text().unwrap().contains("deleted f.txt"));
        assert!(matches!(
            ctx.result::<FileChange>(),
            Some(FileChange::Delete { path, original: Some(content) })
                if path == "f.txt" && content == "gone soon"
        ));
        drop(dir);
    }

    #[tokio::test]
    async fn delete_missing_file_fails() {
        let (dir, _guard) = tempdir();
        let err = delete_file_tool()
            .call(&mut new_ctx(), json!({ "path": "nope.txt" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("delete nope.txt"));
        drop(dir);
    }

    #[tokio::test]
    async fn delete_rejects_directories() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("sub").unwrap();
        let err = delete_file_tool()
            .call(&mut new_ctx(), json!({ "path": "sub" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("is a directory"),
            "{}",
            err.to_string()
        );
        assert!(std::fs::read_dir("sub").unwrap().next().is_none());
        drop(dir);
    }

    #[tokio::test]
    async fn delete_asks_for_hidden_paths() {
        let (dir, _guard) = tempdir();
        std::fs::write(".env", "secret").unwrap();
        let err = delete_file_tool()
            .call(&mut new_ctx(), json!({ "path": ".env" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("permission"),
            "{}",
            err.to_string()
        );
        assert_eq!(std::fs::read_to_string(".env").unwrap(), "secret");
        drop(dir);
    }

    #[tokio::test]
    async fn delete_asks_outside_workspace() {
        let (dir, _guard) = tempdir();
        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-deletable-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        let err = delete_file_tool()
            .call(&mut new_ctx(), json!({ "path": rel }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("permission"),
            "{}",
            err.to_string()
        );
        assert!(outside.exists());
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }

    #[tokio::test]
    async fn delete_change_restores_on_undo_and_removes_on_redo() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "original").unwrap();
        let mut ctx = new_ctx();
        delete_file_tool()
            .call(&mut ctx, json!({ "path": "f.txt" }))
            .await
            .unwrap();
        let change = ctx.result::<FileChange>().unwrap();
        assert_eq!(change.new_content(), None);
        assert_eq!(change.original_content().as_deref(), Some("original"));
        assert!(!std::fs::exists("f.txt").unwrap());
        for (path, original, _new) in change.patch_files() {
            match original {
                Some(text) => std::fs::write(path, text).unwrap(),
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "original");
        for (_path, _original, new) in change.patch_files() {
            if let Some(text) = new {
                std::fs::write(_path, text).unwrap();
            } else {
                let _ = std::fs::remove_file(_path);
            }
        }
        assert!(!std::fs::exists("f.txt").unwrap());
        drop(dir);
    }
}
