use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;

use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::notification::{Progress, PublishDiagnostics};
use async_lsp::lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializedParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, Url, VersionedTextDocumentIdentifier,
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

pub struct Server {
    pub name: String,
    pub language: String,
    pub socket: ServerSocket,
    pub child: Arc<Mutex<tokio::process::Child>>,
    pub main_loop: tokio::task::JoinHandle<()>,
    pub opened_docs: Arc<Mutex<HashSet<Url>>>,
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
            let mut router = Router::new(ClientState {
                on_diagnostics: on_diagnostics.clone(),
            });
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
                .event(|_this, _: Stop| std::ops::ControlFlow::Break(Ok(())));

            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(router)
        });

        let compat_in = stdout.compat();
        let compat_out = stdin.compat_write();

        let main_loop_handle = tokio::spawn(async move {
            if let Err(e) = mainloop.run_buffered(compat_in, compat_out).await {
                eprintln!("LSP main loop ended: {e}");
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
        })
    }

    pub async fn initialize(&self, root_uri: Url) -> Result<(), String> {
        let mut socket = self.socket.clone();
        #[allow(deprecated)]
        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri),
            workspace_folders: None,
            initialization_options: None,
            capabilities: ClientCapabilities::default(),
            trace: None,
            work_done_progress_params: Default::default(),
            client_info: None,
            locale: None,
            root_path: None,
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
        let _ = socket.shutdown(()).await;
        let _ = socket.exit(());
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
