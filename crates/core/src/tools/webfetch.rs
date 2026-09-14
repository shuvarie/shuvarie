use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use super::{DEFAULT_TIMEOUT_SECS, arg_value};

const WEBFETCH_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const WEBFETCH_MAX_TIMEOUT_SECS: u64 = 120;
pub(crate) const WEBFETCH_BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

pub(crate) fn webfetch_accept_header(format: &str) -> &'static str {
    match format {
        "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        "html" => {
            "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1"
        }
        _ => {
            "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1"
        }
    }
}

pub(crate) struct WebFetch {
    max_output_chars: usize,
}

impl WebFetch {
    pub(crate) fn new(max_output_chars: usize) -> Self {
        Self { max_output_chars }
    }
}

impl Tool for WebFetch {
    const NAME: &'static str = "webfetch";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fetch content from a specified URL and return it as text or markdown. \
         HTML pages are converted to the requested format (markdown by default); \
         non-HTML content (plain text, JSON, XML, source code) is returned as-is. \
         Use this to retrieve web pages, API responses, and online documentation."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch content from" },
                "format": { "type": "string", "enum": ["text", "markdown", "html"], "description": "The format to return the content in. Defaults to markdown" },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds (max 120, default 30)" }
            },
            "required": ["url"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let url = arg_value(&args, "url")?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err("URL must start with http:// or https://".to_string());
            }
            let format = args
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("markdown")
                .to_string();
            if !matches!(format.as_str(), "text" | "markdown" | "html") {
                return Err(format!(
                    "invalid format '{format}' (expected text, markdown, or html)"
                ));
            }
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, WEBFETCH_MAX_TIMEOUT_SECS);
            let timeout = std::time::Duration::from_secs(timeout_secs);

            let client = reqwest::Client::builder()
                .user_agent(WEBFETCH_BROWSER_UA)
                .connect_timeout(timeout)
                .timeout(timeout)
                .build()
                .map_err(|e| format!("build http client: {e}"))?;
            let response = client
                .get(&url)
                .header("Accept", webfetch_accept_header(&format))
                .header("Accept-Language", "en-US,en;q=0.9")
                .send()
                .await
                .map_err(|e| format!("request {url}: {e}"))?;

            let status = response.status();
            if status.as_u16() == 403
                && response
                    .headers()
                    .get("cf-mitigated")
                    .and_then(|v| v.to_str().ok())
                    == Some("challenge")
            {
                let retry = client
                    .get(&url)
                    .header("Accept", webfetch_accept_header(&format))
                    .header("User-Agent", "shuvarie")
                    .send()
                    .await
                    .map_err(|e| format!("request {url}: {e}"))?;
                return webfetch_finish(retry, &url, &format, max_output_chars).await;
            }
            if status.is_client_error() || status.is_server_error() {
                return Err(format!("request {url}: HTTP {status}"));
            }
            webfetch_finish(response, &url, &format, max_output_chars).await
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

pub(crate) async fn webfetch_finish(
    response: reqwest::Response,
    url: &str,
    format: &str,
    max_output_chars: usize,
) -> Result<ToolOutput, String> {
    if let Some(len) = response.content_length()
        && len as usize > WEBFETCH_MAX_RESPONSE_BYTES
    {
        return Err(format!(
            "response too large ({len} bytes, exceeds 5 MB limit)"
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    let mut body = Vec::new();
    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| format!("read {url}: {e}"))?
    {
        if body.len() + chunk.len() > WEBFETCH_MAX_RESPONSE_BYTES {
            return Err(format!(
                "response too large (exceeds 5 MB limit) while streaming {url}"
            ));
        }
        body.extend_from_slice(&chunk);
    }

    if mime.starts_with("image/") {
        return Err(format!(
            "{url} returned an image ({mime}); webfetch can only return text content"
        ));
    }

    let looks_html = webfetch_looks_html(&mime, url);
    let text = String::from_utf8_lossy(&body).into_owned();
    let out = if looks_html {
        match format {
            "html" => text,
            _ => webfetch_convert(&text, format == "text"),
        }
    } else {
        text
    };

    let out = out.trim();
    if out.is_empty() {
        return Ok(ToolOutput::text(
            "(empty response — the page may require JavaScript to render)",
        ));
    }

    let body = format!("{url} ({content_type})\n\n{out}");
    if let Some(capped) =
        crate::truncate::truncate_output(&body, max_output_chars, "fetch a narrower URL if needed")
    {
        return Ok(ToolOutput::text(capped));
    }
    Ok(ToolOutput::text(body))
}

fn webfetch_looks_html(mime: &str, url: &str) -> bool {
    if mime.contains("text/html") || mime.contains("application/xhtml") {
        return true;
    }
    if !mime.is_empty() {
        return false;
    }
    url.split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('.').next())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm" | "xhtml"))
}

fn webfetch_convert(html: &str, plain: bool) -> String {
    let options = html_to_markdown_rs::ConversionOptions::builder()
        .bullets("-".to_string())
        .skip_images(true)
        .extract_metadata(false)
        .extract_images(false)
        .compact_tables(true)
        .include_document_structure(false)
        .output_format(if plain {
            html_to_markdown_rs::OutputFormat::Plain
        } else {
            html_to_markdown_rs::OutputFormat::Markdown
        })
        .build();
    html_to_markdown_rs::convert(html, options)
        .map(|result| result.content.unwrap_or_default())
        .unwrap_or_else(|e| format!("(html conversion failed: {e})\n\n{html}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::new_ctx;

    #[tokio::test]
    async fn webfetch_rejects_non_http_urls() {
        for url in ["ftp://example.com/x", "file:///etc/passwd", "example.com"] {
            let err = WebFetch::new(0)
                .call(&mut new_ctx(), json!({ "url": url }))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("http://"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn webfetch_rejects_invalid_format() {
        let err = WebFetch::new(0)
            .call(
                &mut new_ctx(),
                json!({ "url": "https://example.com", "format": "pdf" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid format"), "{err}");
    }

    #[test]
    fn webfetch_looks_html_rules() {
        assert!(webfetch_looks_html(
            "text/html; charset=utf-8",
            "https://x.com"
        ));
        assert!(webfetch_looks_html(
            "application/xhtml+xml",
            "https://x.com"
        ));
        assert!(!webfetch_looks_html("application/json", "https://x.com"));
        assert!(!webfetch_looks_html("text/plain", "https://x.com/a.html"));
        assert!(webfetch_looks_html("", "https://x.com/docs/page.html"));
        assert!(webfetch_looks_html("", "https://x.com/a/b.htm?q=1#frag"));
        assert!(webfetch_looks_html("", "https://x.com/c.XHTML"));
        assert!(!webfetch_looks_html("", "https://x.com/data.json"));
        assert!(!webfetch_looks_html("", "https://x.com/noext"));
    }

    #[test]
    fn webfetch_convert_markdown_and_plain() {
        let html = "<h1>Title</h1><p>Para with <b>bold</b> and a <a href='https://x.com'>link</a>.</p><script>alert(1)</script>";
        let md = webfetch_convert(html, false);
        assert!(md.contains("# Title"), "{md}");
        assert!(md.contains("**bold**"), "{md}");
        assert!(!md.contains("alert(1)"), "{md}");
        let plain = webfetch_convert(html, true);
        assert!(plain.contains("Title"), "{plain}");
        assert!(!plain.contains("**"), "{plain}");
        assert!(!plain.contains("alert(1)"), "{plain}");
    }

    #[tokio::test]
    async fn webfetch_finish_truncates_long_output() {
        let body = "x".repeat(4000);
        let out = webfetch_finish(
            reqwest::Response::from(
                http::Response::builder()
                    .status(200)
                    .header("content-type", "text/plain")
                    .body(body)
                    .unwrap(),
            ),
            "https://example.com/data.txt",
            "text",
            100,
        )
        .await
        .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("output truncated"), "{text}");
        assert!(text.contains("chars omitted"), "{text}");
    }

    #[tokio::test]
    async fn webfetch_finish_rejects_images_and_empty() {
        let err = webfetch_finish(
            reqwest::Response::from(
                http::Response::builder()
                    .status(200)
                    .header("content-type", "image/png")
                    .body(b"\x89PNG".to_vec())
                    .unwrap(),
            ),
            "https://example.com/i.png",
            "text",
            0,
        )
        .await
        .unwrap_err();
        assert!(err.contains("image"), "{err}");

        let out = webfetch_finish(
            reqwest::Response::from(
                http::Response::builder()
                    .status(200)
                    .header("content-type", "text/html")
                    .body(
                        "<html><body><script>var x=1;</script></body></html>"
                            .as_bytes()
                            .to_vec(),
                    )
                    .unwrap(),
            ),
            "https://example.com/e",
            "markdown",
            0,
        )
        .await
        .unwrap();
        assert!(
            out.as_text().unwrap().contains("empty response"),
            "{}",
            out.as_text().unwrap()
        );
    }
}
