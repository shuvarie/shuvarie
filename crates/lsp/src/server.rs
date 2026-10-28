use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::notification::{Progress, PublishDiagnostics};
use async_lsp::lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializedParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, Url, VersionedTextDocumentIdentifier,
    WorkspaceFolder,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::tracing::TracingLayer;
use async_lsp::{LanguageServer, MainLoop, ServerSocket};
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceBuilder;

use crate::types::DiagnosticInfo;

struct ClientState {
    on_diagnostics: tokio::sync::mpsc::UnboundedSender<(Url, Vec<DiagnosticInfo>)>,
}

struct Stop;

/// Number of trailing server stderr lines kept: enough to hold the context
/// around a startup failure or a configuration complaint (the report itself
/// quotes the last one).
const STDERR_TAIL_LINES: usize = 20;

/// The last [`STDERR_TAIL_LINES`] lines the server wrote to its (piped) stderr.
type StderrTail = Arc<StdMutex<std::collections::VecDeque<String>>>;

pub struct Server {
    pub name: String,
    pub language: String,
    pub socket: ServerSocket,
    pub child: Arc<Mutex<tokio::process::Child>>,
    pub main_loop: tokio::task::JoinHandle<()>,
    pub opened_docs: Arc<Mutex<HashSet<Url>>>,
    /// Why the main loop stopped, when it stopped with an error. The TUI owns
    /// the terminal, so the loop must never print: `LspManager` reads this and
    /// reports it as an `Event::LspError` notice instead.
    exit_error: Arc<StdMutex<Option<String>>>,
}

impl Server {
    pub fn spawn(
        name: String,
        language: String,
        command: &[String],
        _root_uri: Url,
        on_diagnostics: tokio::sync::mpsc::UnboundedSender<(Url, Vec<DiagnosticInfo>)>,
    ) -> Result<Self, String> {
        let bin = command
            .first()
            .ok_or_else(|| format!("empty command for LSP server {name}"))?
            .clone();
        let args: Vec<String> = command.iter().skip(1).cloned().collect();

        let mut child = tokio::process::Command::new(&bin);
        child
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut kid = child
            .spawn()
            .map_err(|e| format!("spawn LSP server {name} ({bin} {}): {e}", args.join(" ")))?;

        let stdout = kid
            .stdout
            .take()
            .ok_or_else(|| format!("no stdout from LSP server {name}"))?;
        let stdin = kid
            .stdin
            .take()
            .ok_or_else(|| format!("no stdin from LSP server {name}"))?;

        let (mainloop, server_socket) = MainLoop::new_client(|_server_socket| {
            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(client_router(on_diagnostics.clone()))
        });

        let compat_in = stdout.compat();
        let compat_out = stdin.compat_write();

        // The server's stderr is piped: it must be read or a chatty server
        // blocks once the pipe buffer fills, and it must never reach the
        // terminal, which the TUI owns.
        let stderr_tail: StderrTail = Arc::default();
        if let Some(stderr) = kid.stderr.take() {
            spawn_stderr_drain(stderr, Arc::clone(&stderr_tail));
        }

        let reported_name = name.clone();
        let exit_error: Arc<StdMutex<Option<String>>> = Arc::default();
        let reported_error = Arc::clone(&exit_error);
        let main_loop_handle = tokio::spawn(async move {
            if let Err(e) = mainloop.run_buffered(compat_in, compat_out).await {
                let tail: Vec<String> = stderr_tail
                    .lock()
                    .map(|tail| tail.iter().cloned().collect())
                    .unwrap_or_default();
                if let Ok(mut slot) = reported_error.lock() {
                    *slot = Some(exit_message(&reported_name, &e, &tail));
                }
            }
        });

        let opened_docs = Arc::new(Mutex::new(HashSet::new()));
        Ok(Self {
            name,
            language,
            socket: server_socket,
            child: Arc::new(Mutex::new(kid)),
            main_loop: main_loop_handle,
            opened_docs,
            exit_error,
        })
    }

    /// The error that stopped the main loop, when it stopped with one. `None`
    /// while the connection is alive (or after a clean shutdown).
    pub fn exit_error(&self) -> Option<String> {
        self.exit_error.lock().ok().and_then(|slot| slot.clone())
    }

    pub async fn initialize(&self, root_uri: Url) -> Result<(), String> {
        let mut socket = self.socket.clone();
        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            workspace_folders: Some(vec![WorkspaceFolder {
                name: root_uri.to_string(),
                uri: root_uri,
            }]),
            capabilities: ClientCapabilities::default(),
            work_done_progress_params: Default::default(),
            ..Default::default()
        };
        socket
            .initialize(init_params)
            .await
            .map_err(|e| format!("LSP initialize: {e:?}"))?;
        socket
            .initialized(InitializedParams {})
            .map_err(|e| format!("LSP initialized: {e:?}"))?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let mut socket = self.socket.clone();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), socket.shutdown(())).await;
        let _ = socket.exit(());
        let _ = socket.emit(Stop);
        Ok(())
    }

    pub async fn did_open(&self, uri: &Url, language_id: &str, text: &str) {
        if self.opened_docs.lock().await.contains(uri) {
            self.did_change(uri, text).await;
            return;
        }
        let mut socket = self.socket.clone();
        let _ = socket.did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version: 0,
                text: text.to_string(),
            },
        });
        self.opened_docs.lock().await.insert(uri.clone());
    }

    pub async fn did_change(&self, uri: &Url, text: &str) {
        if !self.opened_docs.lock().await.contains(uri) {
            return;
        }
        let mut socket = self.socket.clone();
        let _ = socket.did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 1,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        });
    }

    pub async fn did_close(&self, uri: &Url) {
        if self.opened_docs.lock().await.remove(uri) {
            let mut socket = self.socket.clone();
            let _ = socket.did_close(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            });
        }
    }

    pub async fn pid(&self) -> Option<u32> {
        self.child.lock().await.id()
    }

    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

/// The client-side router: diagnostics and progress are consumed, and every
/// other notification a server sends is accepted.
///
/// The catch-all matters. `async-lsp`'s default stops the whole main loop with
/// `Unhandled notification: <method>`, and servers routinely send informational
/// notifications (`window/logMessage`, `window/showMessage`,
/// `telemetry/event`, …) that are not protocol-violating. Ending the
/// connection over one would also be fatal to diagnostics, and the only place
/// the resulting error could surface was the TUI's terminal — the stray text
/// this router exists to prevent.
fn client_router(
    on_diagnostics: tokio::sync::mpsc::UnboundedSender<(Url, Vec<DiagnosticInfo>)>,
) -> Router<ClientState> {
    let mut router = Router::new(ClientState { on_diagnostics });
    router
        .notification::<PublishDiagnostics>(|this, params| {
            let diagnostics: Vec<DiagnosticInfo> = params
                .diagnostics
                .iter()
                .map(DiagnosticInfo::from_lsp_types)
                .collect();
            let _ = this.on_diagnostics.send((params.uri.clone(), diagnostics));
            std::ops::ControlFlow::Continue(())
        })
        .notification::<Progress>(|_this, _params| std::ops::ControlFlow::Continue(()))
        .unhandled_notification(|_this, _notification| std::ops::ControlFlow::Continue(()))
        .event(|_this, _: Stop| std::ops::ControlFlow::Break(Ok(())));
    router
}

/// The notice a stopped main loop reports: the error, then the last thing the
/// server itself said — a server that died usually explains why there. Kept to
/// one line: it renders in the session's single-line footer.
fn exit_message(server: &str, error: &async_lsp::Error, stderr_tail: &[String]) -> String {
    let mut message = format!("LSP server '{server}' stopped: {error}");
    if let Some(last) = stderr_tail
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
    {
        message.push_str(": ");
        message.push_str(last);
    }
    // Server output is untrusted text: a control character (an ANSI escape,
    // say) would reach the terminal through the renderer instead of being
    // drawn as a cell.
    message.retain(|ch| !ch.is_control());
    message
}

/// Drain the server's piped stderr, keeping only the last
/// [`STDERR_TAIL_LINES`] lines. Unread, the pipe fills up and blocks a server
/// that logs; passed through, it would corrupt the TUI's screen.
fn spawn_stderr_drain(stderr: tokio::process::ChildStderr, tail: StderrTail) {
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut guard) = tail.lock() else {
                break;
            };
            guard.push_back(line);
            while guard.len() > STDERR_TAIL_LINES {
                guard.pop_front();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use async_lsp::{AnyNotification, LspService};

    use super::*;

    /// Every notification a server may send is accepted: an unhandled one must
    /// not stop the main loop (the default handler breaks it, which used to
    /// print the protocol error onto the TUI).
    #[test]
    fn unhandled_notifications_do_not_stop_the_main_loop() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut service = client_router(tx);
        for method in [
            "window/logMessage",
            "window/showMessage",
            "telemetry/event",
            "workspace/didChangeConfiguration",
            "$/setTrace",
        ] {
            let notification: AnyNotification =
                serde_json::from_value(serde_json::json!({ "method": method }))
                    .expect("a notification frame");
            assert!(
                service.notify(notification).is_continue(),
                "notification `{method}` stopped the main loop"
            );
        }
    }

    /// A stopped loop reports the error and the last thing the server said.
    #[test]
    fn an_exit_message_carries_the_error_and_the_stderr_tail() {
        let error = async_lsp::Error::Protocol("bad header".into());
        assert_eq!(
            exit_message("rust", &error, &[]),
            "LSP server 'rust' stopped: protocol error: bad header"
        );
        assert_eq!(
            exit_message(
                "rust",
                &error,
                &[
                    "panicked at main.rs:1".to_string(),
                    "  no \x1b[31mconfig\x1b[0m  ".to_string(),
                    String::new(),
                ]
            ),
            "LSP server 'rust' stopped: protocol error: bad header: no [31mconfig[0m"
        );
    }
}
