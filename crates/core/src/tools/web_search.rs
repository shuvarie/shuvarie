use serde_json::{Value, json};
use shuvarie_config::{WebSearchConfig, WebSearchKind, WebSearchParamKind, WebSearchParams};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use super::webfetch::{WEBFETCH_BROWSER_UA, webfetch_accept_header, webfetch_finish};
use super::{DEFAULT_TIMEOUT_SECS, arg_value};

const WEB_SEARCH_MAX_TIMEOUT_SECS: u64 = 120;

pub(crate) struct WebSearch {
    url: String,
    kind: WebSearchKind,
    headers: Vec<(String, String)>,
    params: WebSearchParams,
    max_output_chars: usize,
}

impl WebSearch {
    pub(crate) fn new(config: &WebSearchConfig, max_output_chars: usize) -> Self {
        Self {
            url: config.url.clone(),
            kind: config.kind,
            headers: config
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), crate::catalog::resolve_env(value)))
                .collect(),
            params: config.params.clone(),
            max_output_chars,
        }
    }
}

impl Tool for WebSearch {
    const NAME: &'static str = "web_search";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Search the web with the configured web search endpoint and return matching \
         results (titles, URLs, and content snippets). Use for current information, \
         documentation lookup, and fact checking; follow up with webfetch to read a \
         specific result page."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query" },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds (max 120, default 30)" }
            },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let query = arg_value(&args, "query")?;
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, WEB_SEARCH_MAX_TIMEOUT_SECS);
            let timeout = std::time::Duration::from_secs(timeout_secs);

            let client = reqwest::Client::builder()
                .user_agent(WEBFETCH_BROWSER_UA)
                .connect_timeout(timeout)
                .timeout(timeout)
                .build()
                .map_err(|e| format!("build http client: {e}"))?;

            let (request_url, body) = match self.params.kind {
                WebSearchParamKind::BodyJson => (
                    self.url.clone(),
                    Some(search_body_json(&self.params, &query)?),
                ),
                WebSearchParamKind::Query => {
                    (search_query_url(&self.url, &self.params, &query)?, None)
                }
            };

            let mut request = match &body {
                Some(body) => client.post(&request_url).json(body),
                None => client
                    .get(&request_url)
                    .header("Accept", webfetch_accept_header("markdown"))
                    .header("Accept-Language", "en-US,en;q=0.9"),
            };
            for (name, value) in &self.headers {
                request = request.header(name, value);
            }
            let response = request
                .send()
                .await
                .map_err(|e| format!("request {request_url}: {e}"))?;

            match self.kind {
                WebSearchKind::Ollama => {
                    let status = response.status();
                    let text = response
                        .text()
                        .await
                        .map_err(|e| format!("read {request_url}: {e}"))?;
                    if status.is_client_error() || status.is_server_error() {
                        return Err(format!(
                            "request {request_url}: HTTP {status}\n{}",
                            body_snippet(&text)
                        ));
                    }
                    format_ollama_results(&text, max_output_chars)
                }
                WebSearchKind::ToMarkdown => {
                    let status = response.status();
                    if status.is_client_error() || status.is_server_error() {
                        return Err(format!("request {request_url}: HTTP {status}"));
                    }
                    webfetch_finish(response, &request_url, "markdown", max_output_chars).await
                }
            }
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

fn search_body_json(params: &WebSearchParams, query: &str) -> Result<Value, String> {
    let Some(remote) = params.map.get("query") else {
        return Err("params are missing the `query` mapping".to_string());
    };
    let mut body = serde_json::Map::new();
    body.insert(remote.clone(), Value::String(query.to_string()));
    Ok(Value::Object(body))
}

fn search_query_url(base: &str, params: &WebSearchParams, query: &str) -> Result<String, String> {
    let Some(remote) = params.map.get("query") else {
        return Err("params are missing the `query` mapping".to_string());
    };
    let separator = if base.contains('?') { '&' } else { '?' };
    Ok(format!(
        "{base}{separator}{remote}={}",
        percent_encode(query)
    ))
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn format_ollama_results(text: &str, max_output_chars: usize) -> Result<ToolOutput, String> {
    let parsed: Value = serde_json::from_str(text)
        .map_err(|e| format!("parse response JSON: {e} — body: {}", body_snippet(text)))?;
    let results = parsed.get("results").ok_or_else(|| {
        format!(
            "response is missing `results` — body: {}",
            body_snippet(text)
        )
    })?;
    let results = results
        .as_array()
        .ok_or_else(|| format!("`results` is not an array — body: {}", body_snippet(text)))?;
    if results.is_empty() {
        return Ok(ToolOutput::text("No results found."));
    }
    let mut lines = Vec::with_capacity(results.len());
    for (index, result) in results.iter().enumerate() {
        let title = result
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)")
            .trim();
        let url = result
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("(no url)")
            .trim();
        let content = result
            .get("content")
            .and_then(Value::as_str)
            .map(collapse_whitespace)
            .unwrap_or_default();
        let mut line = format!("{}. {title} — {url}", index + 1);
        if !content.is_empty() {
            line.push_str("\n   ");
            line.push_str(&content);
        }
        lines.push(line);
    }
    let body = lines.join("\n\n");
    if let Some(capped) = crate::truncate::truncate_output(
        &body,
        max_output_chars,
        "narrow the query or fetch a result page with webfetch",
    ) {
        return Ok(ToolOutput::text(capped));
    }
    Ok(ToolOutput::text(body))
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn body_snippet(body: &str) -> String {
    const MAX_CHARS: usize = 300;
    let flattened = collapse_whitespace(body.trim());
    if flattened.chars().count() <= MAX_CHARS {
        return flattened;
    }
    let truncated: String = flattened.chars().take(MAX_CHARS).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::new_ctx;
    use shuvarie_config::DUCKDUCKGO_LITE_URL;

    fn config(kind: WebSearchKind) -> WebSearchConfig {
        WebSearchConfig {
            disabled: false,
            url: "https://search.example/api".to_string(),
            kind,
            headers: Default::default(),
            params: WebSearchParams::default_for(kind),
        }
    }

    fn ollama_results() -> &'static str {
        r#"{
            "results": [
                {
                    "title": "Ollama",
                    "url": "https://ollama.com/",
                    "content": "Cloud models are now\n  available\tfor everyone."
                },
                {
                    "title": "What is Ollama?",
                    "url": "https://www.hostinger.com/tutorials/what-is-ollama",
                    "content": "Ariffud M. 6min Read."
                }
            ]
        }"#
    }

    #[test]
    fn percent_encode_encodes_reserved_bytes() {
        assert_eq!(percent_encode("q=a b&c"), "q%3Da%20b%26c");
        assert_eq!(percent_encode("héllo~"), "h%C3%A9llo~");
        assert_eq!(percent_encode("-._"), "-._");
    }

    #[test]
    fn search_query_url_appends_mapped_param() {
        let params = WebSearchParams::default_for(WebSearchKind::ToMarkdown);
        assert_eq!(
            search_query_url(DUCKDUCKGO_LITE_URL, &params, "rust async").unwrap(),
            "https://lite.duckduckgo.com/lite/?q=rust%20async"
        );
        assert_eq!(
            search_query_url("https://a/?x=1", &params, "q").unwrap(),
            "https://a/?x=1&q=q"
        );
    }

    #[test]
    fn search_body_json_maps_remote_name() {
        let params = WebSearchParams::default_for(WebSearchKind::Ollama);
        let body = search_body_json(&params, "rust async").unwrap();
        assert_eq!(body, json!({ "query": "rust async" }));

        let params = WebSearchParams {
            kind: WebSearchParamKind::BodyJson,
            map: [("query".to_string(), "search".to_string())].into(),
        };
        assert_eq!(
            search_body_json(&params, "q").unwrap(),
            json!({ "search": "q" })
        );
    }

    #[tokio::test]
    async fn web_search_rejects_empty_query() {
        let err = WebSearch::new(&config(WebSearchKind::Ollama), 0)
            .call(&mut new_ctx(), json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query"), "{err}");
    }

    #[tokio::test]
    async fn ollama_results_are_formatted() {
        let response = http::Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .body(ollama_results().to_string())
            .unwrap();
        let response = reqwest::Response::from(response);
        let out = format_ollama_results(&response.text().await.unwrap(), 0).unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("1. Ollama — https://ollama.com/"), "{text}");
        assert!(
            text.contains("Cloud models are now available for everyone."),
            "{text}"
        );
        assert!(
            text.contains("2. What is Ollama? — https://www.hostinger.com"),
            "{text}"
        );
        assert!(!text.contains('\t'), "{text}");
    }

    #[tokio::test]
    async fn ollama_results_handle_empty_and_malformed() {
        let out = format_ollama_results(r#"{"results": []}"#, 0).unwrap();
        assert_eq!(out.as_text().unwrap(), "No results found.");

        let err = format_ollama_results("not json", 0).unwrap_err();
        assert!(err.contains("parse response JSON"), "{err}");
        assert!(err.contains("not json"), "{err}");

        let err = format_ollama_results(r#"{"hits": []}"#, 0).unwrap_err();
        assert!(err.contains("missing `results`"), "{err}");

        let err = format_ollama_results(r#"{"results": 5}"#, 0).unwrap_err();
        assert!(err.contains("not an array"), "{err}");
    }

    #[tokio::test]
    async fn ollama_results_truncate_long_output() {
        let many: Value = json!({
            "results": (0..200)
                .map(|i| json!({
                    "title": format!("Result {i}"),
                    "url": format!("https://example.com/{i}"),
                    "content": "x".repeat(400),
                }))
                .collect::<Vec<_>>()
        });
        let out = format_ollama_results(&many.to_string(), 1000).unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("output truncated"), "{text}");
        assert!(text.contains("narrow the query"), "{text}");
    }

    #[test]
    fn body_snippet_flattens_and_caps() {
        assert_eq!(body_snippet("  a\nb\tc  "), "a b c");
        let long = "x".repeat(1000);
        let snippet = body_snippet(&long);
        assert!(snippet.chars().count() == 301, "{snippet}");
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn headers_resolve_env_placeholders() {
        unsafe { std::env::set_var("SHUVARIE_TEST_SEARCH_KEY", "secret-token") };
        let mut cfg = config(WebSearchKind::Ollama);
        cfg.headers.insert(
            "Authorization".to_string(),
            "Bearer $SHUVARIE_TEST_SEARCH_KEY".to_string(),
        );
        let tool = WebSearch::new(&cfg, 0);
        assert_eq!(
            tool.headers.as_slice(),
            [(
                "Authorization".to_string(),
                "Bearer secret-token".to_string()
            )]
        );

        unsafe { std::env::remove_var("SHUVARIE_TEST_SEARCH_KEY") };
        let tool = WebSearch::new(&cfg, 0);
        assert_eq!(
            tool.headers.as_slice(),
            [(
                "Authorization".to_string(),
                "Bearer $SHUVARIE_TEST_SEARCH_KEY".to_string()
            )]
        );
    }
}
