use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::resolve_read;

use super::{ReadCache, arg_value};

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const BINARY_SAMPLE_BYTES: usize = 4096;

pub(crate) struct ReadFile {
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
}

impl ReadFile {
    pub(crate) fn new(
        read_cache: ReadCache,
        max_output_chars: usize,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            read_cache,
            max_output_chars,
            max_output_bytes,
        }
    }
}

impl Tool for ReadFile {
    const NAME: &'static str = "read_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Read a text file from the workspace, optionally restricted to a line range. Returns the requested lines or an error when the path does not exist, is a directory, or contains binary data."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the file, relative to the workspace root" },
                "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based). Defaults to 1" },
                "limit": { "type": "integer", "minimum": 1, "description": format!("Maximum number of lines to read. Defaults to {DEFAULT_READ_LIMIT}") }
            },
            "required": ["path"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let read_cache = self.read_cache.clone();
        let max_output_chars = self.max_output_chars;
        let max_output_bytes = self.max_output_bytes;
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            if read_cache.mark(&path, offset, limit) {
                return Ok(ToolOutput::text(format!(
                    "(already read {path} — see the earlier result; use a different offset/limit to re-read a range)"
                )));
            }
            let abs = resolve_read(&path)?;
            if abs.is_dir() {
                return Err(format!("'{path}' is a directory, not a file"));
            }
            let data = tokio::fs::read(&abs).await.map_err(|e| format!("read {path}: {e}"))?;
            if is_binary_file(&data) {
                return Err(format!("'{path}' appears to be binary; refusing to read"));
            }
            let content_owned = String::from_utf8_lossy(&data).into_owned();
            let lines: Vec<&str> = content_owned.lines().collect();
            let offset = offset.unwrap_or(1).max(1) as usize;
            if offset > lines.len() && !(offset == 1 && lines.is_empty()) {
                return Err(format!(
                    "Offset {offset} is out of range for this file ({} lines)",
                    lines.len()
                ));
            }
            let limit = limit.map(|n| n as usize).unwrap_or(DEFAULT_READ_LIMIT);
            let start = offset - 1;
            let end = (start + limit).min(lines.len());
            let mut out = String::new();
            let mut long_lines = false;
            for (i, line) in lines[start..end].iter().enumerate() {
                let line = if line.chars().count() > MAX_LINE_LENGTH {
                    long_lines = true;
                    let head: String = line.chars().take(MAX_LINE_LENGTH).collect();
                    format!("{head}{MAX_LINE_SUFFIX}")
                } else {
                    line.to_string()
                };
                out.push_str(&format!("{:>6} | {line}\n", start + i + 1));
            }
            let last = start + (end - start);
            let truncated = limit < lines.len() - start;
            if truncated {
                out.push_str(&format!(
                    "\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
                    offset,
                    last,
                    lines.len(),
                    last + 1
                ));
            } else {
                out.push_str(&format!("\n(End of file - total {} lines)", lines.len()));
            }
            if long_lines {
                out.push_str(
                    "\n(long lines truncated; use run_shell, e.g. `sed -n 'Np' file | cut -c1-2000`, to read one exactly)",
                );
            }
            let hint = format!("use offset/limit to read more of {path}");
            if let Some(capped) = crate::truncate::truncate_output(&out, max_output_chars, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            if let Some(capped) = crate::truncate::truncate_bytes(&out, max_output_bytes, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            Ok(ToolOutput::text(out))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

fn is_binary_file(data: &[u8]) -> bool {
    if data.contains(&0) {
        return true;
    }
    let sample = &data[..data.len().min(BINARY_SAMPLE_BYTES)];
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|&&b| b < 9 || (b > 13 && b < 32))
        .count();
    non_printable * 10 > sample.len() * 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::test_util::{new_ctx, tempdir};

    fn read_file_tool() -> ReadFile {
        ReadFile::new(ReadCache::new(), 0, 0)
    }

    #[tokio::test]
    async fn read_file_with_range() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\ntwo\nthree\n").unwrap();
        let out = read_file_tool()
            .call(
                &mut new_ctx(),
                json!({ "path": "a.txt", "offset": 2, "limit": 1 }),
            )
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("two"),
            "{}",
            out.as_text().unwrap()
        );
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "missing.txt" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing.txt"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_refuses_binary() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.dat", [0, 1, 2, 3]).unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "bin.dat" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_truncates_by_bytes() {
        let (dir, _guard) = tempdir();
        std::fs::write("bytes.txt", "abcdef\n").unwrap();
        let tool = ReadFile::new(ReadCache::new(), 0, 3);
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "bytes.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(
            text.contains("bytes omitted") && text.contains("use offset/limit"),
            "{text}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_cache_dedupes_repeated_reads() {
        let (dir, _guard) = tempdir();
        std::fs::write("dup.txt", "line\n").unwrap();
        let cache = ReadCache::new();
        let tool = ReadFile::new(cache.clone(), 0, 0);
        let first = tool
            .call(&mut new_ctx(), json!({ "path": "dup.txt" }))
            .await
            .unwrap();
        assert!(first.as_text().unwrap().contains("line"));
        let second = tool
            .call(&mut new_ctx(), json!({ "path": "dup.txt" }))
            .await
            .unwrap();
        assert!(
            second.as_text().unwrap().contains("already read"),
            "{}",
            second.as_text().unwrap()
        );
        assert!(!second.as_text().unwrap().contains("line |"));
        let ranged = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "dup.txt", "offset": 1, "limit": 1 }),
            )
            .await
            .unwrap();
        assert!(
            ranged.as_text().unwrap().contains("line"),
            "{}",
            ranged.as_text().unwrap()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_truncates_large_output() {
        let (dir, _guard) = tempdir();
        let big = "x".repeat(10_000) + "\n";
        std::fs::write("big.txt", &big).unwrap();
        let tool = ReadFile::new(ReadCache::new(), 100, 0);
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "big.txt" }))
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("truncated"),
            "{}",
            out.as_text().unwrap()
        );
        assert!(out.as_text().unwrap().chars().count() < big.len() + 200);
        drop(dir);
    }

    #[tokio::test]
    async fn read_defaults_to_2000_lines_with_footers() {
        let (dir, _guard) = tempdir();
        let content: String = (1..=2100).map(|i| format!("line{i}\n")).collect();
        std::fs::write("many.txt", &content).unwrap();
        let tool = read_file_tool();
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "many.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Showing lines 1-2000 of 2100"), "{text}");
        assert!(text.contains("Use offset=2001"), "{text}");
        let rest = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "many.txt", "offset": 2001 }),
            )
            .await
            .unwrap();
        let text = rest.as_text().unwrap();
        assert!(text.contains("End of file - total 2100 lines"), "{text}");
        assert!(text.contains("2100 | line2100"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn read_offset_out_of_range_errors() {
        let (dir, _guard) = tempdir();
        std::fs::write("small.txt", "a\nb\nc\n").unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "small.txt", "offset": 10 }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "{}",
            err.to_string()
        );
        assert!(err.to_string().contains("3 lines"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_caps_long_lines() {
        let (dir, _guard) = tempdir();
        std::fs::write("long.txt", "z".repeat(3000) + "\n").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "long.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains(MAX_LINE_SUFFIX), "{text}");
        assert!(text.chars().count() < 2200);
        drop(dir);
    }

    #[tokio::test]
    async fn read_detects_binary_by_nonprintable_ratio() {
        let (dir, _guard) = tempdir();
        let mut data = vec![b'a'; 600];
        data.extend(std::iter::repeat_n(0x07u8, 600));
        std::fs::write("weird.bin", &data).unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "weird.bin" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "{}", err);
        drop(dir);
    }

    #[tokio::test]
    async fn read_reaches_beyond_64kb() {
        let (dir, _guard) = tempdir();
        let line = "y".repeat(1000) + "\n";
        std::fs::write("wide.txt", line.repeat(100)).unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "wide.txt", "offset": 90 }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("90 |"), "{text}");
        assert!(text.contains("End of file - total 100 lines"), "{text}");
        drop(dir);
    }
}
