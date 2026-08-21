use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024;

pub struct LoadedContext {
    pub files: Vec<String>,
    pub content: String,
}

impl LoadedContext {
    fn new(files: Vec<String>, content: String) -> Self {
        Self { files, content }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

pub fn load(root: &Path) -> LoadedContext {
    let mut files: Vec<String> = Vec::new();
    let mut content = String::new();
    let mut remaining = MAX_TOTAL_BYTES;

    let agents_md = root.join("AGENTS.md");
    if agents_md.is_file() {
        push_file(
            "AGENTS.md",
            &agents_md,
            &mut files,
            &mut content,
            &mut remaining,
        );
    }

    let context_dir = root.join(".shuvarie").join("context");
    if context_dir.is_dir() && remaining > 0 {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&context_dir) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    paths.push(entry.path());
                }
            }
        }
        paths.sort();
        for path in paths {
            if remaining == 0 {
                break;
            }
            let rel = match path.strip_prefix(root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) => path.to_string_lossy().into_owned(),
            };
            push_file(&rel, &path, &mut files, &mut content, &mut remaining);
        }
    }

    LoadedContext::new(files, content)
}

fn push_file(
    rel: &str,
    path: &Path,
    files: &mut Vec<String>,
    content: &mut String,
    remaining: &mut usize,
) {
    if *remaining == 0 {
        return;
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return,
    };
    if data.contains(&0) {
        return;
    }
    let truncated = data.len() > MAX_FILE_BYTES;
    let text = String::from_utf8_lossy(&data[..data.len().min(MAX_FILE_BYTES)]);
    let chunk = text.chars().take(*remaining).collect::<String>();
    if chunk.is_empty() {
        return;
    }
    *remaining -= chunk.len();
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(&format!("=== {rel} ===\n{chunk}"));
    if truncated {
        content.push_str("\n… (file truncated)");
    }
    if *remaining == 0 {
        content.push_str("\n… (context budget exhausted)");
    }
    files.push(rel.to_string());
}

pub fn load_from_cwd() -> LoadedContext {
    match std::env::current_dir() {
        Ok(cwd) => load(&cwd),
        Err(_) => LoadedContext::new(Vec::new(), String::new()),
    }
}

pub fn build_preamble(base: &str, context: &LoadedContext) -> String {
    if context.is_empty() {
        base.to_string()
    } else {
        format!(
            "{base}\n\nProject context files are loaded from the workspace. Read them carefully \
             and follow their instructions; they describe the project's structure, conventions, \
             and workflows.\n\n{}",
            context.content
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn tempdir() -> (TempDir, MutexGuard<'static, ()>) {
        let guard = CWD_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    #[test]
    fn loads_agents_md_and_context_dir_sorted() {
        let (dir, _guard) = tempdir();
        std::fs::write("AGENTS.md", "project rules").unwrap();
        std::fs::create_dir_all(".shuvarie/context").unwrap();
        std::fs::write(".shuvarie/context/b.md", "bee").unwrap();
        std::fs::write(".shuvarie/context/a.md", "aye").unwrap();
        std::fs::write(".shuvarie/context/skip.bin", "\0binary").unwrap();

        let ctx = load_from_cwd();
        assert_eq!(
            ctx.files,
            vec![
                "AGENTS.md",
                ".shuvarie/context/a.md",
                ".shuvarie/context/b.md"
            ]
        );
        assert!(ctx.content.contains("=== AGENTS.md ===\nproject rules"));
        assert!(ctx.content.contains("=== .shuvarie/context/a.md ===\naye"));
        assert!(ctx.content.contains("=== .shuvarie/context/b.md ===\nbee"));
        assert!(!ctx.content.contains("binary"));
        drop(dir);
    }

    #[test]
    fn empty_without_files() {
        let (dir, _guard) = tempdir();
        let ctx = load_from_cwd();
        assert!(ctx.is_empty());
        assert!(ctx.files.is_empty());
        assert!(ctx.content.is_empty());
        drop(dir);
    }

    #[test]
    fn missing_agents_only_context_dir() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".shuvarie/context").unwrap();
        std::fs::write(".shuvarie/context/notes.md", "notes").unwrap();
        let ctx = load_from_cwd();
        assert_eq!(ctx.files, vec![".shuvarie/context/notes.md"]);
        assert!(ctx.content.contains("notes"));
        drop(dir);
    }

    #[test]
    fn caps_file_size_and_total_budget() {
        let (dir, _guard) = tempdir();
        let big = "x".repeat(MAX_FILE_BYTES + 100);
        std::fs::write("AGENTS.md", &big).unwrap();
        let ctx = load_from_cwd();
        assert!(ctx.content.starts_with("=== AGENTS.md ===\n"));
        assert!(ctx.content.contains("… (file truncated)"));
        assert!(ctx.content.len() < MAX_TOTAL_BYTES);

        let ctx = LoadedContext::new(vec!["a".into(), "b".into()], "content".into());
        let preamble = build_preamble("base", &ctx);
        assert!(preamble.contains("base"));
        assert!(preamble.contains("content"));
        drop(dir);
    }
}
