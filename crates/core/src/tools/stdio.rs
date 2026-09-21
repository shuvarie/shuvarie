//! The user-configured one-shot subprocess tools (`tools { tool … }`): every
//! call spawns a fresh process running the templated `cmd` argv and returns
//! its stdout. Nothing stays alive between calls.
//!
//! argv elements may reference declared params with `{{param}}` templates —
//! substituted here, never through a shell, so a param value is always
//! exactly one argv element. With `input "json"` the resolved args object is
//! additionally piped to the child's stdin.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::Access;

use super::run_shell::KillGuard;
use super::workspace_root;
use crate::truncate::truncate_output;

/// How much of a stream is captured from a child process before the tail is
/// dropped, mirroring `run_shell`'s capture cap.
const CAPTURE_BYTES: usize = 64 * 1024;

/// A configured subprocess tool.
pub(crate) struct StdioTool {
    /// The agent-facing name from config (`tool name="…"`).
    name: String,
    description: String,
    /// The full templated argv from config; `{{param}}` placeholders are
    /// substituted per call.
    cmd: Vec<String>,
    /// Declared params, driving the JSON schema and template substitution.
    params: BTreeMap<String, shuvarie_config::ToolParam>,
    /// Pipe the resolved args object to the child's stdin as JSON.
    input_json: bool,
    timeout_secs: u64,
    /// The resolved child environment: whether to inherit the parent's, plus
    /// the `$VAR`-resolved entries.
    envs: (bool, BTreeMap<String, String>),
    max_output_chars: usize,
    access: Access,
}

impl StdioTool {
    pub(crate) fn new(
        name: String,
        config: &shuvarie_config::StdioToolConfig,
        max_output_chars: usize,
        access: Access,
    ) -> Self {
        let envs = (
            config.envs.inherit,
            config
                .envs
                .entries
                .iter()
                .map(|(name, value)| (name.clone(), crate::catalog::resolve_env(value)))
                .collect(),
        );
        Self {
            description: config.description.clone().unwrap_or_else(|| {
                format!(
                    "Run the configured `{name}` tool: spawns `{program}` as a subprocess and \
                     returns its stdout.",
                    program = config.cmd.first().map(String::as_str).unwrap_or("?"),
                )
            }),
            name,
            cmd: config.cmd.clone(),
            params: config.params.clone(),
            input_json: config.input.is_some(),
            timeout_secs: config.timeout_secs,
            envs,
            max_output_chars,
            access,
        }
    }

    /// The agent-facing JSON schema built from the declared params.
    fn input_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, param) in &self.params {
            let mut property = serde_json::Map::new();
            property.insert(
                "type".to_string(),
                Value::String(param.kind.as_str().to_string()),
            );
            if let Some(description) = &param.description {
                property.insert(
                    "description".to_string(),
                    Value::String(description.clone()),
                );
            }
            properties.insert(name.clone(), Value::Object(property));
            if param.required {
                required.push(Value::String(name.clone()));
            }
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }

    /// Validates the call arguments against the declared params and renders
    /// the templated argv. With `input "json"` the args object is the child's
    /// payload, so extra keys are legitimate (they flow to stdin); without it,
    /// an undeclared key would be silently dropped, so it is rejected.
    /// Required params must be present; values are type-checked and
    /// stringified for argv.
    fn resolve_call(&self, args: &Value) -> Result<(Vec<String>, Option<Vec<u8>>), String> {
        let object = args
            .as_object()
            .ok_or_else(|| "arguments must be a JSON object".to_string())?;
        if !self.input_json {
            for key in object.keys() {
                if !self.params.contains_key(key) {
                    let declared = self.params.keys().cloned().collect::<Vec<_>>().join("', '");
                    return Err(format!(
                        "unknown parameter '{key}' (declared: {})",
                        if declared.is_empty() {
                            "none".to_string()
                        } else {
                            format!("'{declared}'")
                        }
                    ));
                }
            }
        }
        let mut values = BTreeMap::new();
        for (name, param) in &self.params {
            let Some(value) = object.get(name) else {
                if param.required {
                    return Err(format!("missing required parameter '{name}'"));
                }
                continue;
            };
            let rendered = render_value(name, &param.kind, value)?;
            values.insert(name.clone(), rendered);
        }
        let mut argv = Vec::with_capacity(self.cmd.len());
        for element in &self.cmd {
            argv.push(apply_template(element, &values)?);
        }
        let stdin = self
            .input_json
            .then(|| serde_json::to_vec(args).expect("args serialize"));
        Ok((argv, stdin))
    }

    /// Runs one call: authorize, spawn, pipe optional stdin, capture, and
    /// return stdout (truncated) or an error carrying the streams' tails.
    async fn run(&self, args: Value) -> Result<ToolOutput, ToolExecutionError> {
        let name = self.name.clone();
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let (argv, stdin) = self.resolve_call(&args)?;
            let detail = format!("Command: `{}`", argv.join(" "));
            self.access.authorize_tool(&name, &detail).await?;

            let mut builder = tokio::process::Command::new(&argv[0]);
            builder.args(&argv[1..]);
            if self.envs.0 {
                for (key, value) in &self.envs.1 {
                    builder.env(key, value);
                }
            } else {
                builder.env_clear();
                builder.envs(self.envs.1.iter());
            }
            builder
                .current_dir(workspace_root()?)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(if stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                });
            #[cfg(unix)]
            builder.process_group(0);
            let mut child = builder
                .spawn()
                .map_err(|e| format!("spawn `{}`: {e}", argv[0]))?;
            let pgid = child.id();
            let mut guard = KillGuard::new(pgid);

            if let Some(stdin) = stdin {
                let mut pipe = child.stdin.take().expect("piped stdin");
                use tokio::io::AsyncWriteExt;
                pipe.write_all(&stdin)
                    .await
                    .map_err(|e| format!("stdin: {e}"))?;
                pipe.shutdown().await.map_err(|e| format!("stdin: {e}"))?;
            }

            let mut stdout_pipe = child.stdout.take().expect("piped stdout");
            let mut stderr_pipe = child.stderr.take().expect("piped stderr");
            let capture = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                let mut buf_out = [0u8; 8192];
                let mut buf_err = [0u8; 8192];
                // Interleave both pipes to per-stream caps. Each pipe is read
                // to its own EOF: an early close of one stream (a quiet child
                // exiting fast) must not discard the other's pending output.
                let mut stdout_open = true;
                let mut stderr_open = true;
                while (stdout_open && stdout.len() < CAPTURE_BYTES)
                    || (stderr_open && stderr.len() < CAPTURE_BYTES)
                {
                    tokio::select! {
                        read = stdout_pipe.read(&mut buf_out),
                            if stdout_open && stdout.len() < CAPTURE_BYTES =>
                        {
                            match read {
                                Ok(0) | Err(_) => stdout_open = false,
                                Ok(n) => stdout.extend_from_slice(&buf_out[..n]),
                            }
                        }
                        read = stderr_pipe.read(&mut buf_err),
                            if stderr_open && stderr.len() < CAPTURE_BYTES =>
                        {
                            match read {
                                Ok(0) | Err(_) => stderr_open = false,
                                Ok(n) => stderr.extend_from_slice(&buf_err[..n]),
                            }
                        }
                    }
                }
                (stdout, stderr)
            });
            let wait = tokio::spawn(async move {
                let status = child.wait().await.map_err(|e| format!("wait: {e}"))?;
                Ok::<_, String>(status)
            });
            let (stdout, stderr, status) =
                match tokio::time::timeout(Duration::from_secs(self.timeout_secs), async move {
                    let (stdout, stderr) = capture.await.expect("capture task");
                    let status = wait.await.expect("wait task");
                    let status = status?;
                    Ok::<_, String>((stdout, stderr, status))
                })
                .await
                {
                    Ok(Ok((stdout, stderr, status))) => {
                        guard.disarm();
                        (stdout, stderr, status)
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(_) => {
                        let message = format!(
                            "tool `{name}` timed out after {}s (the process group was killed)",
                            self.timeout_secs
                        );
                        return Err(message);
                    }
                };

            let stdout = String::from_utf8_lossy(&stdout).into_owned();
            let stderr = String::from_utf8_lossy(&stderr).into_owned();
            if !status.success() {
                return Err(format!(
                    "tool `{name}` exited with {status}{}\n{}",
                    stream_note("stdout", &stdout),
                    stream_note("stderr", &stderr),
                ));
            }
            let text =
                match truncate_output(&stdout, max_output_chars, "increase the tool's capture") {
                    Some(truncated) => truncated,
                    None => stdout,
                };
            Ok(ToolOutput::text(text))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// Renders one argument value for argv substitution: type-checked against the
/// declared kind, then stringified.
fn render_value(
    name: &str,
    kind: &shuvarie_config::ToolParamKind,
    value: &Value,
) -> Result<String, String> {
    use shuvarie_config::ToolParamKind as Kind;
    match kind {
        Kind::String => value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("parameter '{name}' must be a string")),
        Kind::Integer => value
            .as_i64()
            .map(|n| n.to_string())
            .ok_or_else(|| format!("parameter '{name}' must be an integer")),
        Kind::Number => {
            let rendered = match value {
                Value::Number(number) => number.to_string(),
                _ => return Err(format!("parameter '{name}' must be a number")),
            };
            Ok(rendered)
        }
        Kind::Boolean => value
            .as_bool()
            .map(|b| if b { "true" } else { "false" }.to_string())
            .ok_or_else(|| format!("parameter '{name}' must be a boolean")),
    }
}

/// Substitutes `{{name}}` placeholders with the resolved param values. Every
/// placeholder was validated against the declared params at parse time; a
/// declared-but-absent optional param is an error (an empty substitution
/// would silently corrupt the command line).
fn apply_template(element: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(element.len());
    let mut rest = element;
    while let Some(start) = rest.find("{{") {
        let (head, tail) = rest.split_at(start + 2);
        out.push_str(head.trim_end_matches("{{"));
        match tail.find("}}") {
            Some(end) => {
                let name = &tail[..end];
                let value = values.get(name).ok_or_else(|| {
                    format!(
                        "parameter '{name}' is referenced in the command but was not provided \
                         (it is optional)"
                    )
                })?;
                out.push_str(value);
                rest = &tail[end + 2..];
            }
            None => {
                out.push_str("{{");
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// A short tail of a captured stream for error messages.
fn stream_note(label: &str, text: &str) -> String {
    let tail: String = text.trim().chars().rev().take(1024).collect();
    let tail: String = tail.chars().rev().collect();
    if tail.is_empty() {
        format!("{label}: (empty)")
    } else {
        format!("{label}: {tail}")
    }
}

impl Tool for StdioTool {
    /// Not the agent-facing name — registration passes the config-derived
    /// name to `into_dynamic` explicitly; the trait name is never consulted
    /// on the dynamic-tool path.
    const NAME: &'static str = "stdio_tool";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        self.input_schema()
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        self.run(args).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shuvarie_config::{ToolParam, ToolParamKind};

    use crate::test_util::{access_for_config, access_with_answering_gate};

    use super::*;

    fn tool(cmd: Vec<&str>, params: Vec<(&str, ToolParam)>) -> StdioTool {
        tool_with(
            cmd,
            params,
            shuvarie_config::PermissionsConfig {
                // Tools authorize through the top-level default verb: allow
                // so the spawn tests skip the ask.
                default: Some(shuvarie_config::Verb::Allow),
                ..shuvarie_config::PermissionsConfig::builtin()
            },
        )
    }

    fn tool_with(
        cmd: Vec<&str>,
        params: Vec<(&str, ToolParam)>,
        permissions: shuvarie_config::PermissionsConfig,
    ) -> StdioTool {
        StdioTool::new(
            "fetch-json".to_string(),
            &shuvarie_config::StdioToolConfig {
                description: None,
                cmd: cmd.into_iter().map(str::to_string).collect(),
                input: None,
                timeout_secs: 30,
                params: params
                    .into_iter()
                    .map(|(name, param)| (name.to_string(), param))
                    .collect(),
                envs: shuvarie_config::EnvsConfig::default(),
            },
            4096,
            access_for_config(&permissions),
        )
    }

    fn param(kind: ToolParamKind, required: bool) -> ToolParam {
        ToolParam {
            kind,
            required,
            description: None,
        }
    }

    /// Reads a successful output as text.
    fn output_text(output: ToolOutput) -> String {
        output.as_text().expect("text output").to_string()
    }

    #[test]
    fn template_substitutes_and_type_checks() {
        let tool = tool(
            vec!["echo", "{{greeting}}", "{{n}}", "{{flag}}"],
            vec![
                ("greeting", param(ToolParamKind::String, true)),
                ("n", param(ToolParamKind::Integer, true)),
                ("flag", param(ToolParamKind::Boolean, false)),
            ],
        );
        let (argv, stdin) = tool
            .resolve_call(&json!({"greeting": "hi there", "n": 3, "flag": true}))
            .expect("resolve");
        assert_eq!(argv, vec!["echo", "hi there", "3", "true"]);
        assert!(stdin.is_none());
    }

    #[test]
    fn template_rejects_unknown_missing_and_wrongly_typed_args() {
        let tool = tool(
            vec!["echo", "{{greeting}}"],
            vec![("greeting", param(ToolParamKind::String, true))],
        );
        let err = tool
            .resolve_call(&json!({"nope": "x"}))
            .expect_err("unknown key");
        assert!(err.contains("unknown parameter 'nope'"), "got: {err}");

        let err = tool.resolve_call(&json!({})).expect_err("missing required");
        assert!(
            err.contains("missing required parameter 'greeting'"),
            "got: {err}"
        );

        let err = tool
            .resolve_call(&json!({"greeting": 4}))
            .expect_err("wrong type");
        assert!(err.contains("must be a string"), "got: {err}");
    }

    #[test]
    fn template_referenced_optional_param_must_be_provided() {
        let tool = tool(
            vec!["echo", "{{count}}"],
            vec![("count", param(ToolParamKind::Integer, false))],
        );
        let err = tool.resolve_call(&json!({})).expect_err("absent");
        assert!(
            err.contains("'count' is referenced in the command but was not provided"),
            "got: {err}"
        );
    }

    #[test]
    fn template_non_placeholder_text_stays_literal() {
        let tool = tool(vec!["echo", "a{ b c}d {{", "close }} only"], vec![]);
        let (argv, _) = tool.resolve_call(&json!({})).expect("resolve");
        assert_eq!(argv, vec!["echo", "a{ b c}d {{", "close }} only"]);
    }

    #[test]
    fn input_json_tolerates_undeclared_keys() {
        // With `input "json"` the args object is the child's payload: keys
        // need no declaration (the schema is the declaration).
        let tool = StdioTool {
            input_json: true,
            ..tool(vec!["cat"], vec![])
        };
        let (argv, stdin) = tool
            .resolve_call(&json!({"query": "hi", "limit": 2}))
            .expect("resolve");
        assert_eq!(argv, vec!["cat"]);
        assert_eq!(
            String::from_utf8(stdin.expect("stdin")).unwrap(),
            r#"{"limit":2,"query":"hi"}"#
        );
    }

    #[test]
    fn input_schema_lists_types_required_and_descriptions() {
        let tool = tool(
            vec!["echo"],
            vec![
                (
                    "path",
                    ToolParam {
                        kind: ToolParamKind::String,
                        required: true,
                        description: Some("Where to look".to_string()),
                    },
                ),
                ("count", param(ToolParamKind::Integer, false)),
            ],
        );
        let schema = tool.parameters();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(
            schema["properties"]["path"],
            json!({"type": "string", "description": "Where to look"})
        );
        assert_eq!(schema["properties"]["count"], json!({"type": "integer"}));
        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(
            tool.description(),
            "Run the configured `fetch-json` tool: spawns `echo` as a subprocess and returns its stdout."
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawns_and_returns_stdout() {
        let tool = tool(
            vec!["echo", "hello {{name}}"],
            vec![("name", param(ToolParamKind::String, true))],
        );
        let out = tool.run(json!({"name": "world"})).await.expect("run");
        assert_eq!(output_text(out).trim(), "hello world");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_exit_reports_streams_and_status() {
        let tool = tool(vec!["sh", "-c", "echo out; echo err >&2; exit 3"], vec![]);
        let err = tool.run(json!({})).await.expect_err("non-zero exit");
        let message = err.to_string();
        assert!(message.contains("exited with"), "got: {message}");
        assert!(message.contains("stdout: out"), "got: {message}");
        assert!(message.contains("stderr: err"), "got: {message}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_and_reports() {
        let tool = tool(vec!["sleep", "10"], vec![]);
        let tool = StdioTool {
            timeout_secs: 1,
            ..tool
        };
        let err = tool.run(json!({})).await.expect_err("timeout");
        assert!(err.to_string().contains("timed out after 1s"), "got: {err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn input_json_pipes_args_to_stdin() {
        let tool = StdioTool {
            input_json: true,
            ..tool(vec!["cat"], vec![])
        };
        let out = tool
            .run(json!({"query": "hi", "limit": 2}))
            .await
            .expect("run");
        assert_eq!(output_text(out).trim(), r#"{"limit":2,"query":"hi"}"#);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn envs_are_resolved_and_inheritance_is_selectable() {
        let mut tool = tool(vec!["sh", "-c", "echo $SHUVARIE_STDIO_TEST"], vec![]);
        tool.envs = (
            false,
            BTreeMap::from([("SHUVARIE_STDIO_TEST".to_string(), "plain-value".to_string())]),
        );
        let out = tool.run(json!({})).await.expect("run");
        assert_eq!(output_text(out).trim(), "plain-value");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_call_asks_and_session_grant_covers_the_name() {
        let (access, mut requests) =
            access_with_answering_gate(&shuvarie_config::PermissionsConfig::builtin());
        let tool = tool_with(
            vec!["echo", "asked"],
            vec![],
            shuvarie_config::PermissionsConfig::builtin(),
        );
        let tool = StdioTool { access, ..tool };
        // The call pauses on the ask; the answerer future must send the verdict
        // while `run` is still parked on it, so `join!` polls both together.
        let (result, _request) = tokio::join!(tool.run(json!({})), async {
            let request = requests.recv().await.expect("permission request");
            assert!(
                request.description.contains("Allow tool `fetch-json`?"),
                "got: {}",
                request.description
            );
            assert_eq!(
                request.scope,
                Some(crate::permissions::AskScope::Tool("fetch-json".to_string()))
            );
            request
                .respond
                .send(crate::permissions::PermissionAnswer::AllowSession)
                .expect("answer sent");
        });
        result.expect("allowed");

        // The session grant covers the tool name: the next call runs without
        // a new ask.
        assert!(requests.try_recv().is_err(), "no new ask after the grant");
        tool.run(json!({})).await.expect("covered by the grant");
        assert!(requests.try_recv().is_err(), "still no ask");
    }
}
