use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind};
use crate::walk;

use super::{arg_value, display_path};

const DEFAULT_MAX_RESULTS: usize = 200;

/// Bytes of a matching line to keep. A minified bundle is a single line of
/// megabytes, and one such line must not fill the whole answer.
const MAX_LINE_BYTES: usize = 512;

/// How much of a file is sniffed for NUL bytes before it is called binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Extensions that are binary whatever their content happens to hold, so a
/// binary file is skipped without being read at all.
const BINARY_EXTENSIONS: &[&str] = &[
    // Images
    "png",
    "jpg",
    "jpeg",
    "gif",
    "bmp",
    "ico",
    "webp",
    "tiff",
    "tif",
    "avif",
    "heic",
    "heif",
    "jxl",
    "jp2",
    "psd",
    "icns",
    "tga",
    // GPU / VFX textures
    "hdr",
    "exr",
    "dds",
    "ktx",
    "ktx2",
    "pvr",
    "astc",
    // Audio / video
    "mp4",
    "avi",
    "mov",
    "wmv",
    "mkv",
    "mp3",
    "wav",
    "flac",
    "ogg",
    "m4a",
    "aac",
    "webm",
    "mpg",
    "mpeg",
    "opus",
    // Archives / packages
    "zip",
    "tar",
    "gz",
    "bz2",
    "xz",
    "7z",
    "rar",
    "zst",
    "lz4",
    "cab",
    "deb",
    "rpm",
    "dmg",
    "msi",
    "iso",
    "whl",
    // Executables / libraries / runtime images
    "exe",
    "dll",
    "so",
    "dylib",
    "o",
    "a",
    "lib",
    "bin",
    "class",
    "pyc",
    "pyo",
    "wasm",
    "jar",
    // Documents
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    // Databases
    "db",
    "sqlite",
    "sqlite3",
    "mdb",
    "sqlite-wal",
    "sqlite-shm",
    "ldb",
    // Fonts
    "ttf",
    "otf",
    "woff",
    "woff2",
    "eot",
    // Serialized data and ML artefacts
    "npy",
    "npz",
    "h5",
    "hdf5",
    "pt",
    "onnx",
    "safetensors",
    "tflite",
    "gguf",
    "parquet",
    "arrow",
    "pb",
    "glb",
    "blend",
];

pub(crate) struct Grep {
    access: Access,
}

impl Grep {
    pub(crate) fn new(access: Access) -> Self {
        Self { access }
    }
}

impl Tool for Grep {
    const NAME: &'static str = "grep";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fast content search: matches a case-sensitive regular expression against text files under `path`, honouring .gitignore inside git repositories and skipping binary files and hidden entries (a hidden `path` itself is still searched). Prints `path:line: content` sorted by path then line, with lines truncated past 512 bytes; results are capped (default 200)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for (Rust regex syntax, case-sensitive)" },
                "path": { "type": "string", "description": "Directory or file to search, relative to the workspace root (defaults to the workspace root)" },
                "include": { "type": "string", "description": "Only search files matching this glob (e.g. '*.rs', 'src/**/*.ts'); a plain string like 'test' matches file names containing it" },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum number of matching lines (default 200)" }
            },
            "required": ["pattern"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let access = self.access.clone();
            let pattern = arg_value(&args, "pattern")?;
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string();
            let include = args
                .get("include")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let max = args
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_RESULTS as u64)
                .max(1) as usize;
            let abs = access.resolve_read(&path)?;
            access.authorize_path(PathKind::Read, &abs, &path).await?;

            tokio::task::spawn_blocking(move || {
                search(&abs, &path, &pattern, include.as_deref(), max)
            })
            .await
            .map_err(|e| format!("grep: {e}"))
            .and_then(|inner| inner)
            .map(ToolOutput::text)
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// One grep call: the shared walk ([`crate::walk`]) lists the files, a
/// `regex::bytes` matcher reads them.
///
/// Nothing is cached between calls — the walk runs per call and the files are
/// read as they are found — so a file the agent edited since the previous call
/// is searched in its current state, not the state the last scan saw.
fn search(
    abs: &Path,
    display_root: &str,
    pattern: &str,
    include: Option<&str>,
    max: usize,
) -> Result<String, String> {
    // The tool's contract is a case-sensitive regular expression over lines, so
    // the pattern reaches the matcher as written: `multi_line` makes `^`/`$`
    // anchors line-relative, and byte mode keeps a non-UTF-8 file searchable.
    let regex = regex::bytes::RegexBuilder::new(pattern)
        .multi_line(true)
        .unicode(false)
        .build()
        .map_err(|e| format!("bad pattern: {e}"))?;

    // The include glob is matched against the path the result prints, so
    // `include: "src/**/*.rs"` says the same whether `path` is the workspace
    // root or `src`. It stays authoritative: a call that narrows to nothing
    // reports "no matches" rather than falling back to every file.
    let include_matcher = match include.map(include_to_glob) {
        Some(glob) => Some(
            globset::Glob::new(&glob)
                .map_err(|e| format!("bad include glob '{glob}': {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    // A single-file `path` searches exactly that file; a directory searches
    // what the walk finds under it. The walk answers sorted by path, so the
    // answer below is a stable prefix of it.
    let candidates: Vec<(String, PathBuf)> = if abs.is_file() {
        vec![(display_root.to_string(), abs.to_path_buf())]
    } else {
        walk::files(abs)
            .into_iter()
            .map(|file| (display_path(&file.rel, display_root), file.abs))
            .collect()
    };

    let mut hits: Vec<(String, usize, String)> = Vec::new();
    let mut truncated = false;
    for (index, (display, path)) in candidates.iter().enumerate() {
        if include_matcher
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(display))
        {
            continue;
        }
        // A file that vanished between the walk and the read, or one this
        // process may not open, is skipped: the rest of the tree is still worth
        // searching.
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if is_binary(path, &bytes) {
            continue;
        }
        // The closing line terminator belongs to the last line, not to a line of
        // its own: without this the empty tail would make a complete answer look
        // like a truncation.
        let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        let mut lines = body.split(|&byte| byte == b'\n').peekable();
        let mut line_number = 0;
        while let Some(line) = lines.next() {
            line_number += 1;
            if !regex.is_match(line) {
                continue;
            }
            hits.push((display.clone(), line_number, line_content(line)));
            if hits.len() >= max {
                // The cap stops the scan, so whatever is left unread could hold
                // more matches: report the truncation. Reaching the cap on the
                // very last line of the very last file is the one case that is
                // a complete answer.
                truncated = index + 1 < candidates.len() || lines.peek().is_some();
                break;
            }
        }
        if truncated {
            break;
        }
    }

    if hits.is_empty() {
        return Ok(format!("no matches for /{pattern}/ in {display_root}"));
    }

    let mut lines = String::new();
    for (display, line_number, content) in &hits {
        lines.push_str(&format!("{display}:{line_number}:{content}\n"));
    }
    if truncated {
        lines.push_str(&format!("… (results truncated at {max})\n"));
    }
    Ok(lines)
}

/// The include filter as a glob: a bare fragment such as `test` or `.rs` is a
/// name substring, anything that already looks like a pattern is left alone.
fn include_to_glob(include: &str) -> String {
    if include.contains('/') || include.contains(['*', '?', '[', '{']) {
        include.to_string()
    } else {
        format!("*{include}*")
    }
}

/// Whether the file is binary. The extension is a cheap first guess; content is
/// the check that catches everything the list does not name.
fn is_binary(path: &Path, bytes: &[u8]) -> bool {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    if extension.is_some_and(|extension| BINARY_EXTENSIONS.contains(&extension.as_str())) {
        return true;
    }
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|&byte| byte == 0)
}

/// One matched line, ready to print: no line terminator, at most
/// [`MAX_LINE_BYTES`] bytes, cut on a character boundary.
fn line_content(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let end = if line.len() <= MAX_LINE_BYTES {
        line.len()
    } else {
        let mut end = MAX_LINE_BYTES;
        // A UTF-8 continuation byte starts with `10…`, i.e. a signed byte below
        // -0x40; stepping back over them lands on a character boundary.
        while end > 0 && (line[end] as i8) < -0x40 {
            end -= 1;
        }
        end
    };
    String::from_utf8_lossy(&line[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    async fn grep(args: Value) -> Result<String, String> {
        Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), args)
            .await
            .map(|out| out.as_text().unwrap().to_string())
            .map_err(|err| err.to_string())
    }

    #[tokio::test]
    async fn grep_finds_with_substring_include() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let text = grep(json!({ "pattern": "fn", "include": ".rs" }))
            .await
            .unwrap();
        assert!(text.contains("r.rs:1"), "{text}");
        assert!(!text.contains("r.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_caps_results() {
        let (dir, _guard) = tempdir();
        let content: String = (0..10).map(|i| format!("line {i} fn\n")).collect();
        std::fs::write("lines.rs", content).unwrap();
        let text = grep(json!({ "pattern": "fn", "max_results": 3 }))
            .await
            .unwrap();
        assert!(text.contains("lines.rs:1"), "{text}");
        assert!(text.contains("lines.rs:3"), "{text}");
        assert!(!text.contains("lines.rs:4"), "{text}");
        assert!(text.contains("truncated"), "{text}");
        drop(dir);
    }

    /// The cap is a truncation only when it actually leaves matches behind; an
    /// answer that ends exactly at the cap is complete and says so.
    #[tokio::test]
    async fn grep_does_not_call_an_exact_cap_a_truncation() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "fn 1\nfn 2\n").unwrap();
        let text = grep(json!({ "pattern": "fn", "max_results": 2 }))
            .await
            .unwrap();
        assert!(text.contains("a.txt:2"), "{text}");
        assert!(!text.contains("truncated"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_respects_gitignore_inside_a_repository() {
        let (dir, _guard) = tempdir();
        // A `.gitignore` only applies inside a repository; for the walk an
        // existing `.git` is what marks one.
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".gitignore", "ignored.txt\n").unwrap();
        std::fs::write("ignored.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains("ignored.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_skips_hidden_outside_a_repository() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir(".hidden").unwrap();
        std::fs::write(".hidden/secret.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        drop(dir);
    }

    /// A dot entry is hidden inside a repository too: `.env` and `.shuvarie/`
    /// never reach the model implicitly.
    #[tokio::test]
    async fn grep_skips_hidden_inside_a_repository() {
        let (dir, _guard) = tempdir();
        // An empty `.git` is enough for the walk to treat the root as a repo.
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".env", "needle=1\n").unwrap();
        std::fs::create_dir(".shuvarie").unwrap();
        std::fs::write(".shuvarie/session.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains(".env"), "{text}");
        assert!(!text.contains("session.txt"), "{text}");
        drop(dir);
    }

    /// A `path` that names a hidden entry is explicit, so it is searched (the
    /// permission layer still asks, as it should: dot directories are gated).
    #[tokio::test]
    async fn grep_searches_an_explicit_hidden_path() {
        use crate::test_util::access_with_answering_gate;

        let (dir, _guard) = tempdir();
        std::fs::create_dir(".config").unwrap();
        std::fs::write(".config/keys.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let (access, mut requests) =
            access_with_answering_gate(&shuvarie_config::PermissionsConfig::builtin());
        let tool = Grep::new(access);
        let mut ctx = new_ctx();
        let (result, ()) = tokio::join!(
            tool.call(&mut ctx, json!({ "pattern": "needle", "path": ".config" })),
            async {
                let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    tokio::task::yield_now().await;
                    requests.recv().await
                })
                .await
                .expect("no permission request within 5s")
                .expect("permission request");
                request
                    .respond
                    .send(crate::permissions::PermissionAnswer::AllowSession)
                    .expect("answer sent");
            }
        );
        let out = result.unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains(".config/keys.txt:1"), "{text}");
        assert!(!text.contains("kept.txt"), "{text}");
        drop(dir);
    }

    /// There is no size cap: a file past any "reasonable" limit answers with its
    /// matches instead of being skipped.
    #[tokio::test]
    async fn grep_searches_a_file_past_the_usual_caps() {
        let (dir, _guard) = tempdir();
        let mut content = String::with_capacity(11 * 1024 * 1024);
        for _ in 0..(11 * 1024) {
            content.push_str(&"x".repeat(1023));
            content.push('\n');
        }
        content.push_str("needle\n");
        std::fs::write("big.txt", content).unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("big.txt:11265"), "{text}");
        drop(dir);
    }

    /// The answer is sorted by path then line, not walk order, so repeated calls
    /// over unchanged files agree.
    #[tokio::test]
    async fn grep_output_is_sorted_by_path_and_line() {
        let (dir, _guard) = tempdir();
        std::fs::write("b.txt", "needle 1\nneedle 2\n").unwrap();
        std::fs::write("a.txt", "needle 3\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        let order: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("needle"))
            .collect();
        assert_eq!(
            order,
            vec!["a.txt:1:needle 3", "b.txt:1:needle 1", "b.txt:2:needle 2"],
            "{text}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn grep_include_glob_matches_subdirs() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("src").unwrap();
        std::fs::write("src/a.rs", "needle\n").unwrap();
        std::fs::write("src/b.txt", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle", "include": "*.rs" }))
            .await
            .unwrap();
        assert!(text.contains("src/a.rs:1"), "{text}");
        assert!(!text.contains("b.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_skips_binary_files() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.png", "needle\n").unwrap();
        std::fs::write("text.dat", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("text.dat:1"), "{text}");
        assert!(!text.contains("bin.png"), "{text}");
        drop(dir);
    }

    /// A file whose extension says nothing but whose bytes hold a NUL is binary
    /// too: the answer must not carry raw bytes into the transcript.
    #[tokio::test]
    async fn grep_skips_binary_content_without_a_known_extension() {
        let (dir, _guard) = tempdir();
        std::fs::write("blob", b"needle\x00\x01\x02\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("no matches"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_single_file_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "needle\nother\n").unwrap();
        std::fs::write("b.txt", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle", "path": "a.txt" }))
            .await
            .unwrap();
        assert!(text.contains("a.txt:1"), "{text}");
        assert!(!text.contains("b.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_is_case_sensitive() {
        let (dir, _guard) = tempdir();
        std::fs::write("c.txt", "Needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        assert!(text.contains("no matches"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_reports_bad_pattern() {
        let (dir, _guard) = tempdir();
        let err = grep(json!({ "pattern": "([" })).await.unwrap_err();
        assert!(err.contains("bad pattern"), "{err}");
        drop(dir);
    }

    /// A `path` or `include` that matches nothing reports exactly that: the
    /// filter is authoritative instead of falling back to every file.
    #[tokio::test]
    async fn grep_include_stays_authoritative_on_a_zero_match_glob() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "needle\n").unwrap();
        std::fs::write("b.rs", "needle\n").unwrap();
        let text = grep(json!({ "pattern": "needle", "include": "*.kdl" }))
            .await
            .unwrap();
        assert!(text.contains("no matches"), "{text}");
        assert!(!text.contains("a.txt"), "{text}");
        assert!(!text.contains("b.rs"), "{text}");
        drop(dir);
    }

    /// The pattern reaches the regex engine as written: no query layer rewrites
    /// a leading `\`, so `\*ptr` stays a literal asterisk.
    #[tokio::test]
    async fn grep_keeps_an_escaped_leading_metacharacter() {
        let (dir, _guard) = tempdir();
        std::fs::write("c.txt", "a *ptr deref\nplain ptr\n").unwrap();
        let text = grep(json!({ "pattern": "\\*ptr" })).await.unwrap();
        assert!(text.contains("c.txt:1:a *ptr deref"), "{text}");
        assert!(!text.contains("plain ptr"), "{text}");
        drop(dir);
    }

    /// A minified file is one enormous line; the answer keeps a readable prefix
    /// of it instead of the whole thing.
    #[tokio::test]
    async fn grep_truncates_a_long_line() {
        let (dir, _guard) = tempdir();
        let line = format!("needle{}\n", "x".repeat(2000));
        std::fs::write("min.js", line).unwrap();
        let text = grep(json!({ "pattern": "needle" })).await.unwrap();
        let content = text.lines().next().unwrap();
        assert_eq!(content.len(), "min.js:1:".len() + MAX_LINE_BYTES, "{text}");
        drop(dir);
    }
}
