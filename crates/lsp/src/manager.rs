use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::config::LspServerSpec;
use crate::registry;
use crate::server::Server;
use crate::types::{DiagnosticInfo, DiagnosticMap, DiagnosticUpdate, LspStatus, ServerStatus};
use crate::util::{language_for_extension, matches_filter, to_file_url};

pub struct LspManager {
    workspace_root: PathBuf,
    enabled: bool,
    specs: BTreeMap<String, LspServerSpec>,
    servers: BTreeMap<String, Server>,
    diagnostics: DiagnosticMap,
    diagnostics_rx:
        tokio::sync::mpsc::UnboundedReceiver<(async_lsp::lsp_types::Url, Vec<DiagnosticInfo>)>,
    diagnostics_tx:
        tokio::sync::mpsc::UnboundedSender<(async_lsp::lsp_types::Url, Vec<DiagnosticInfo>)>,
    supervisor: Option<JoinHandle<()>>,
    on_status: tokio::sync::mpsc::UnboundedSender<()>,
}

impl LspManager {
    pub fn new(
        workspace_root: PathBuf,
        enabled: bool,
        specs: BTreeMap<String, LspServerSpec>,
    ) -> Self {
        let (diag_tx, diag_rx) = tokio::sync::mpsc::unbounded_channel();
        let (status_tx, _status_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        Self {
            workspace_root,
            enabled,
            specs,
            servers: BTreeMap::new(),
            diagnostics: DiagnosticMap::new(),
            diagnostics_rx: diag_rx,
            diagnostics_tx: diag_tx,
            supervisor: None,
            on_status: status_tx,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn enabled_mut(&mut self) -> &mut bool {
        &mut self.enabled
    }

    pub fn specs(&self) -> &BTreeMap<String, LspServerSpec> {
        &self.specs
    }

    pub fn diagnostics(&self) -> &DiagnosticMap {
        &self.diagnostics
    }

    pub fn has_active_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    pub fn start_supervisor(&mut self) {
        if self.supervisor.is_some() {
            return;
        }
        let mut rx = std::mem::replace(&mut self.diagnostics_rx, {
            let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
            rx
        });
        // We cannot easily mutate self from the supervisor (borrow); instead, we let the
        // supervisor relay diagnostics to a channel the core task polls. For simplicity
        // here, we drain in-band on each public call via `pump_diagnostics`.
        self.supervisor = Some(tokio::spawn(async move {
            while let Some((uri, diags)) = rx.recv().await {
                // Forwarded to a no-op here; core task calls pump_diagnostics instead.
                let _ = uri;
                let _ = diags;
            }
        }));
    }

    pub async fn shutdown_all(&mut self) {
        let names: Vec<String> = self.servers.keys().cloned().collect();
        for name in names {
            let _ = self.stop(&name).await;
        }
        if let Some(handle) = self.supervisor.take() {
            handle.abort();
        }
    }

    pub async fn pump_diagnostics(&mut self) -> Vec<DiagnosticUpdate> {
        let mut updates = Vec::new();
        while let Ok((uri, diags)) = self.diagnostics_rx.try_recv() {
            let path = uri
                .to_file_path()
                .ok()
                .and_then(|p| {
                    p.strip_prefix(&self.workspace_root)
                        .ok()
                        .map(|p| p.to_path_buf())
                })
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| uri.to_string());
            updates.push(DiagnosticUpdate {
                path: path.clone(),
                diagnostics: diags.clone(),
            });
            if diags.is_empty() {
                self.diagnostics.remove(&path);
            } else {
                self.diagnostics.insert(path, diags);
            }
        }
        updates
    }

    pub fn status_snapshot(&self) -> Vec<LspStatus> {
        self.servers
            .iter()
            .map(|(name, srv)| {
                let pid = srv.child.try_lock().ok().and_then(|c| c.id());
                LspStatus {
                    name: name.clone(),
                    language: srv.language.clone(),
                    status: ServerStatus::Running,
                    pid,
                    diagnostics: self.diagnostics.values().map(|v| v.len()).sum(),
                    error: None,
                }
            })
            .collect()
    }

    pub async fn start(&mut self, name: &str) -> Result<(), String> {
        if !self.enabled {
            return Err("LSP is disabled in config".into());
        }
        if self.servers.contains_key(name) {
            return Ok(());
        }
        let spec = self
            .specs
            .get(name)
            .ok_or_else(|| format!("no LSP server configured for language '{name}'"))?
            .clone();
        if spec.command.is_empty() {
            return Err(format!("LSP server '{name}' has an empty command"));
        }
        let bin = &spec.command[0];
        if registry::probe(bin).is_none() {
            return Err(format!(
                "LSP server '{name}': binary '{bin}' not found on PATH"
            ));
        }
        let root_uri = to_file_url(&self.workspace_root)
            .ok_or_else(|| "invalid workspace root".to_string())?;
        let server = Server::spawn(
            name.to_string(),
            name.to_string(),
            &spec.command,
            root_uri.clone(),
            self.diagnostics_tx.clone(),
        )?;
        server.initialize(root_uri).await?;
        self.servers.insert(name.to_string(), server);
        let _ = self.on_status.send(());
        Ok(())
    }

    pub async fn stop(&mut self, name: &str) -> Result<(), String> {
        let Some(server) = self.servers.remove(name) else {
            return Ok(());
        };
        let _ = server.shutdown().await;
        server.kill().await;
        server.main_loop.abort();
        let _ = self.on_status.send(());
        Ok(())
    }

    pub async fn restart(&mut self, name: &str) -> Result<(), String> {
        self.stop(name).await?;
        self.start(name).await
    }

    pub fn list(&self, all: bool, filter: Option<&str>) -> Vec<ServerListEntry> {
        let active: BTreeMap<&String, &Server> = self.servers.iter().collect();
        let mut out = Vec::new();
        if all {
            for (lang, spec) in &self.specs {
                let is_active = active.contains_key(lang);
                if let Some(f) = filter
                    && !matches_filter(lang, lang, f)
                {
                    continue;
                }
                out.push(ServerListEntry {
                    name: lang.clone(),
                    language: lang.clone(),
                    command: spec.command.clone(),
                    status: if is_active {
                        ServerListStatus::Running
                    } else {
                        ServerListStatus::Configured
                    },
                });
            }
        } else {
            for (name, srv) in &self.servers {
                if let Some(f) = filter
                    && !matches_filter(name, &srv.language, f)
                {
                    continue;
                }
                out.push(ServerListEntry {
                    name: name.clone(),
                    language: srv.language.clone(),
                    command: self
                        .specs
                        .get(name)
                        .map(|s| s.command.clone())
                        .unwrap_or_default(),
                    status: ServerListStatus::Running,
                });
            }
        }
        out
    }

    pub async fn on_file_open(&mut self, path: &Path, content: &str) {
        if !self.enabled {
            return;
        }
        let Some(lang) = language_for_extension(&self.specs, path) else {
            return;
        };
        let spec = match self.specs.get(&lang) {
            Some(s) => s,
            None => return,
        };
        if !spec.auto_start {
            return;
        }
        if !self.servers.contains_key(&lang) {
            if registry::probe(&spec.command[0]).is_none() {
                return;
            }
            if self.start(&lang).await.is_err() {
                return;
            }
        }
        let Some(server) = self.servers.get(&lang) else {
            return;
        };
        let Some(uri) = to_file_url(path) else {
            return;
        };
        server.did_open(&uri, &lang, content).await;
    }

    pub async fn on_file_change(&mut self, path: &Path, content: &str) {
        if !self.enabled {
            return;
        }
        let Some(lang) = language_for_extension(&self.specs, path) else {
            return;
        };
        let spec = match self.specs.get(&lang) {
            Some(s) => s,
            None => return,
        };
        if !spec.auto_start {
            return;
        }
        if !self.servers.contains_key(&lang) {
            if registry::probe(&spec.command[0]).is_none() {
                return;
            }
            if self.start(&lang).await.is_err() {
                return;
            }
        }
        let Some(server) = self.servers.get(&lang) else {
            return;
        };
        let Some(uri) = to_file_url(path) else {
            return;
        };
        server.did_change(&uri, content).await;
    }

    pub async fn on_file_close(&mut self, path: &Path) {
        if !self.enabled {
            return;
        }
        let Some(lang) = language_for_extension(&self.specs, path) else {
            return;
        };
        let Some(server) = self.servers.get(&lang) else {
            return;
        };
        if let Some(uri) = to_file_url(path) {
            server.did_close(&uri).await;
        }
    }

    pub fn diagnostics_for(&self, path: &str) -> Vec<DiagnosticInfo> {
        self.diagnostics.get(path).cloned().unwrap_or_default()
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        if let Some(handle) = self.supervisor.take() {
            handle.abort();
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerListEntry {
    pub name: String,
    pub language: String,
    pub command: Vec<String>,
    pub status: ServerListStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerListStatus {
    Running,
    Configured,
}

impl ServerListStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ServerListStatus::Running => "running",
            ServerListStatus::Configured => "configured (not running)",
        }
    }
}

pub type SharedManager = Arc<Mutex<LspManager>>;
