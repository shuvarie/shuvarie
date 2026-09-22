use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use shuvarie_db::{LockAcquire, SESSION_LOCK_HEARTBEAT_MS, Store};
use shuvarie_llm::{DeviceCodeHandler, FileChange, ProviderClient, TokenUsage};

use crate::command::Command;
use crate::core_task::steer::SteerSignal;
use crate::embeddings::{self, EmbeddingSetup};
use crate::event::Event;
use crate::permissions::{Access, DenyCut, PermissionAnswer, PermissionGate, PermissionRequest};
use crate::question::{AnswerResponse, QuestionGate, QuestionRequest};
use crate::session::Session;
use crate::shell::Shell;
use shuvarie_config::Config;
use shuvarie_config::{Connections, ProviderConfig, TitleConfig, TrustGrants};

mod steer;
#[cfg(test)]
mod tests;

/// How a streamed turn ended, reported back to the run loop so it can decide
/// whether to auto-continue after a context overflow.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamOutcome {
    /// The turn completed normally (or was cancelled/errored).
    Finished,
    /// The turn was cut at an action boundary so a queued steered prompt
    /// could take over. The partial output is persisted as interrupted.
    Preempted,
    /// The turn was stopped because the context budget overflowed. `compacted`
    /// is true when the session history was successfully summarized so the
    /// next turn can continue with `[summary, tail]`.
    Overflowed { compacted: bool },
    /// The turn failed with a retryable connection failure (timeout, reset,
    /// refused, HTTP 408/429/5xx). The run loop schedules an auto-retry
    /// (resuming the turn) after a backoff, up to `[retry].max-retries`.
    ConnectionLost { reason: String, message: String },
}

/// A scheduled connection retry: when it fires plus the original error
/// message (re-emitted as `StreamError` if the user cancels the wait).
struct PendingRetry {
    deadline: tokio::time::Instant,
    message: String,
}

/// Escalating retry delay schedule: 3s, 5s, 10s, 20s, 30s, then 60s for
/// every further attempt.
struct RetrySchedule;

impl RetrySchedule {
    const DELAYS_SECS: [u64; 5] = [3, 5, 10, 20, 30];
    const MAX_DELAY_SECS: u64 = 60;

    fn delay_secs(attempt: usize) -> u64 {
        Self::DELAYS_SECS
            .get(attempt.saturating_sub(1))
            .copied()
            .unwrap_or(Self::MAX_DELAY_SECS)
    }
}

/// Which turn action the main stream is currently in, tracked so a steered
/// prompt can cut in when the running action completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionPhase {
    /// Nothing streamed yet this turn: the first action cannot be preempted.
    Fresh,
    /// A reasoning segment is streaming.
    Thinking,
    /// Text deltas are streaming.
    Text,
    /// A tool batch is running (some calls started, results pending).
    Tools,
    /// The last action completed — the whole tool batch settled and surfaced
    /// its results. The next main-stream item starting any action is the
    /// boundary at which a queued steered prompt dispatches.
    Between,
}

/// Whether the agent loop is occupied: a live stream or a scheduled
/// connection retry. Steered prompts queue while this is true.
fn is_busy(ctx: &CoreCtx, pending_retry: Option<&PendingRetry>) -> bool {
    ctx.active_stream.as_ref().is_some_and(|h| !h.is_finished()) || pending_retry.is_some()
}

/// Switches the active session's scene: refuses while a turn is busy, while
/// the name does not resolve to a configured scene (`None` = the built-in
/// Default, always switchable via its hard-coded interlude), and — once the
/// session has messages — for configured scenes without an interlude: the
/// injected prompt is what tells the model the scene changed, so an
/// interlude-less scene can only start a session. A switch before the first
/// message picks the scene the session will start under (recording it even
/// when no session exists yet). Only the in-memory scene moves here: the DB
/// persist is deferred to the first request under the new scene, which is
/// also where the interlude is injected once (`self_replay_send`). Reports
/// `SceneChanged`.
async fn switch_scene(
    ctx: &mut CoreCtx,
    pending_retry: Option<&PendingRetry>,
    name: Option<String>,
) -> Option<String> {
    if is_busy(ctx, pending_retry) {
        return Some("a turn is in flight; wait for it to finish".to_string());
    }
    let switchable = match &name {
        Some(name) => match ctx.scenes.scene(name) {
            Some(config) => crate::scenes::is_switchable(config),
            None => return Some(format!("unknown scene `{name}`")),
        },
        // The built-in default scene carries the hard-coded default interlude.
        None => true,
    };
    let Some(s) = &ctx.session else {
        // Before the first turn: only a concrete scene needs recording — an
        // unpicked session would apply `scenes.default` at its first turn.
        let scene = name?;
        let session = Arc::new(Mutex::new(Session::new()));
        session.lock().await.scene = Some(scene.clone());
        ctx.session = Some(session);
        let _ = ctx.event_tx.send(Event::SessionStarted).await;
        let _ = ctx
            .event_tx
            .send(Event::SceneChanged { name: Some(scene) })
            .await;
        return None;
    };
    let (current, has_messages) = {
        let guard = s.lock().await;
        (guard.scene.clone(), !guard.messages.is_empty())
    };
    if current == name {
        return None;
    }
    if has_messages && !switchable {
        let name = name.expect("an unswitchable scene is configured");
        return Some(format!(
            "scene `{name}` has no interlude; it cannot be entered mid-session"
        ));
    }
    {
        let mut guard = s.lock().await;
        guard.scene = name.clone();
    }
    let _ = ctx.event_tx.send(Event::SceneChanged { name }).await;
    None
}

/// Decide the next connection-retry step: `None` when retrying is disabled
/// (`max_retries == 0`) or the cap is reached (give up), otherwise the
/// 1-based attempt number and its delay.
fn next_connection_retry(attempts_so_far: usize, max_retries: usize) -> Option<(usize, u64)> {
    let attempt = attempts_so_far.checked_add(1)?;
    if attempt > max_retries {
        return None;
    }
    Some((attempt, RetrySchedule::delay_secs(attempt)))
}

/// Which session (if any) to load when the core task starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartupSession {
    #[default]
    None,
    /// Resume the most recently updated session (`-c` / `--current`).
    MostRecent,
    /// Resume the session with the given UUID (`-s` / `--session`).
    Session(uuid::Uuid),
}

/// Shared state owned by the core task's run loop, threaded through the
/// turn-streaming helpers. Bundles the long-lived run-loop state plus the
/// per-turn parameters derived once from config, so the helpers take a single
/// `&mut CoreCtx` instead of a long parameter list.
struct CoreCtx {
    store: Store,
    connections: Connections,
    clients: HashMap<String, ProviderClient>,
    embedding_setup: Option<EmbeddingSetup>,
    lsp: std::sync::Arc<tokio::sync::Mutex<shuvarie_lsp::LspManager>>,
    /// The configured MCP servers; connected lazily on first use, tools
    /// bridged into the roster per turn.
    mcp: shuvarie_mcp::SharedMcpManager,
    active_stream: Option<AbortHandle>,
    turn_state: Option<Arc<Mutex<TurnState>>>,
    event_tx: Sender<Event>,
    stream_done_tx: Sender<StreamOutcome>,
    question_tx: Sender<QuestionRequest>,
    access: Access,
    config: Config,
    /// The workspace trust decision made at startup: which categories load.
    trust: TrustGrants,
    workspace_root: PathBuf,
    shell: Shell,
    /// Whether `Event::ContextLoaded` was already sent for the current
    /// session; context files are announced once per chat, not per request.
    context_announced: bool,
    skills: crate::skills::Skills,
    session: Option<Arc<Mutex<Session>>>,
    locked_session: Option<uuid::Uuid>,
    manager_turns: usize,
    worker_turns: usize,
    max_output_chars: usize,
    max_output_bytes: usize,
    steer: SteerSignal,
    /// The configured scene set (config chain + `scene.d` drop-ins).
    scenes: shuvarie_config::ScenesConfig,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    connections: Connections,
    store: Store,
    startup: StartupSession,
    config_path: Option<PathBuf>,
    connections_path: Option<PathBuf>,
    permissions: Arc<crate::permissions::Permissions>,
    trust: TrustGrants,
    scene_set: shuvarie_config::SceneSet,
    mut cmd_rx: Receiver<Command>,
    event_tx: Sender<Event>,
) {
    let store = store.with_client_id(uuid::Uuid::now_v7().to_string());
    let mut semantic_search: Option<AbortHandle> = None;
    let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel::<StreamOutcome>(1);
    let mut overflow_retries: usize = 0;
    const MAX_OVERFLOW_RETRIES: usize = 3;

    // Connection-failure auto-retry state: consecutive failures within one
    // retry chain, plus the currently scheduled wait (if any).
    let mut conn_retries: usize = 0;
    let mut pending_retry: Option<PendingRetry> = None;

    // Steered prompts: submissions made while the agent loop is busy. Queued
    // in order; the front is dispatched as the next user turn at the next
    // completed action boundary of the active stream (after a tool batch
    // settles or after a thinking/text segment — the stream task then cuts
    // the turn), when the turn finishes, or when it is cancelled. The back is
    // what Alt+Up recalls (its model override is dropped there — the recalled
    // text is re-submitted by the user).
    let mut steered: Vec<(String, Option<String>)> = Vec::new();
    let steer = SteerSignal::default();

    let (question_tx, mut question_rx) = tokio::sync::mpsc::channel::<QuestionRequest>(8);
    let mut pending_questions: HashMap<u64, oneshot::Sender<AnswerResponse>> = HashMap::new();
    let mut next_question_id: u64 = 0;

    // Permission asks from the gated tools (`ask` verdicts): forwarded to
    // the TUI as `Event::PermissionRequested`, resolved by
    // `Command::PermissionDecide`.
    let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(8);
    let mut pending_permissions: HashMap<u64, oneshot::Sender<PermissionAnswer>> = HashMap::new();
    let mut next_permission_id: u64 = 0;
    let access = Access::new(
        permissions,
        PermissionGate::new(permission_tx),
        DenyCut::default(),
    );

    // Bash-mode (`!`) runs, routed back to the TUI by id.
    let mut next_bash_id: u64 = 0;

    let mut clients: HashMap<String, ProviderClient> = HashMap::new();
    // Refresh the provider catalog from the hosted service before wiring up
    // clients, so pricing/context/embedding lookups see fresh data. Only when
    // the selune registry is remote-first and enabled: the offline-first
    // default skips the fetch (the popups fetch on demand via Ctrl+O) and a
    // disabled registry never fetches. Bounded so an unreachable catalog
    // never blocks startup.
    let selune_registry = config.registries.selune();
    if !selune_registry.disabled && selune_registry.remote_first {
        let refresh = tokio::task::spawn_blocking(crate::catalog::refresh);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), refresh)
            .await
            .map(|r| r.unwrap_or_default())
            .unwrap_or_default();
    }
    let embedding_setup = embeddings::setup(&config, &connections, &mut clients, &event_tx);
    if let Some(setup) = embedding_setup.clone() {
        let store_backfill = store.clone();
        tokio::spawn(async move {
            embeddings::backfill(&mut store_backfill.clone(), &setup).await;
        });
    }

    let workspace_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let lsp_config = crate::lsp_manager::lsp_config_from_repr(&config.lsp);
    let lsp = std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_lsp::LspManager::new(
        workspace_root.clone(),
        lsp_config.enabled,
        lsp_config.resolve(),
    )));
    let mcp = crate::mcp_manager::mcp_manager(&config.tools.mcp);
    let mut lsp_pump_tick = tokio::time::interval(std::time::Duration::from_millis(500));
    lsp_pump_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Don't fire immediately on the first tick.
    lsp_pump_tick.reset();

    let mut lock_beat =
        tokio::time::interval(std::time::Duration::from_millis(SESSION_LOCK_HEARTBEAT_MS));
    lock_beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    lock_beat.reset();

    let skills = crate::skills::Skills::load(&workspace_root, &config.skills, &trust);
    let _ = event_tx
        .send(Event::SkillsLoaded {
            skills: skills.skills.clone(),
            warnings: skills.warnings.clone(),
        })
        .await;

    let custom_commands = crate::custom_commands::CustomCommands::load(&workspace_root);
    let _ = event_tx
        .send(Event::CustomCommandsLoaded {
            commands: custom_commands.commands.clone(),
            warnings: custom_commands.warnings.clone(),
        })
        .await;

    let shell_resolution = crate::shell::resolve(config.shell.path.as_deref());
    if let Some(warning) = shell_resolution.warning {
        let _ = event_tx
            .send(Event::ShellWarning { message: warning })
            .await;
    }
    let shell = shell_resolution.shell;

    let scene_list = {
        let mut entries = vec![crate::scenes::SceneListEntry {
            id: None,
            name: crate::scenes::DEFAULT_SCENE_NAME.to_string(),
            description: Some(crate::scenes::DEFAULT_SCENE_DESCRIPTION.to_string()),
            switchable: true,
        }];
        for (name, scene) in &scene_set.scenes.scenes {
            entries.push(crate::scenes::SceneListEntry {
                id: Some(name.clone()),
                name: name.clone(),
                description: scene.description.clone(),
                switchable: crate::scenes::is_switchable(scene),
            });
        }
        entries
    };
    let _ = event_tx
        .send(Event::ScenesLoaded {
            scenes: scene_list,
            default: scene_set.scenes.default.clone(),
            warnings: scene_set.warnings,
        })
        .await;

    // Seed the sidebar's MCP section: every configured server shows as
    // `Configured` until its first connect (no I/O here — just the specs).
    let _ = event_tx
        .send(Event::McpStatus {
            servers: mcp.lock().await.status_snapshot(),
        })
        .await;

    let manager_turns = config.agent.effective_max_turns();
    let worker_turns = config.agent.effective_worker_max_turns();
    let max_output_chars = config.context.tool_output_max_chars;
    let max_output_bytes = config.context.tool_output_max_bytes;
    let mut ctx = CoreCtx {
        store,
        connections,
        clients,
        embedding_setup,
        lsp,
        mcp,
        active_stream: None,
        turn_state: None,
        event_tx,
        stream_done_tx,
        question_tx,
        access,
        config,
        trust,
        workspace_root,
        shell,
        context_announced: false,
        skills,
        session: None,
        locked_session: None,
        manager_turns,
        worker_turns,
        max_output_chars,
        max_output_bytes,
        steer,
        scenes: scene_set.scenes,
    };
    load_startup_session(
        &mut ctx.store,
        &mut ctx.session,
        &mut ctx.locked_session,
        &ctx.event_tx,
        startup,
    )
    .await;

    loop {
        let retry_deadline = pending_retry.as_ref().map(|p| p.deadline);
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Command::Ping => {
                        let _ = ctx.event_tx.send(Event::Pong).await;
                    }
                    Command::FetchRegistry => {
                        let fetch = tokio::task::spawn_blocking(crate::catalog::fetch_remote);
                        let outcome =
                            match tokio::time::timeout(std::time::Duration::from_secs(15), fetch)
                                .await
                            {
                                Ok(Ok(Ok(providers))) => Ok(providers),
                                Ok(Ok(Err(error))) => Err(error),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(_) => Err("registry fetch timed out".to_string()),
                            };
                        let _ = match outcome {
                            Ok(providers) => {
                                ctx.event_tx.send(Event::RegistryLoaded { providers }).await
                            }
                            Err(error) => ctx.event_tx.send(Event::RegistryError { error }).await,
                        };
                    }
                    Command::ListModels { provider_name } => {
                        let client = match client_for(
                            &mut ctx.clients,
                            &mut ctx.connections,
                            &ctx.event_tx,
                            &provider_name,
                        ) {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::ModelsError {
                                        provider_name,
                                        error: e,
                                    })
                                    .await;
                                continue;
                            }
                        };
                        match client.list_models().await {
                            Ok(mut models) => {
                                let providers = crate::catalog::providers();
                                let provider = ctx
                                    .connections
                                    .providers
                                    .get(&provider_name)
                                    .and_then(|pc| pc.catalog_id())
                                    .and_then(|id| crate::catalog::find_provider(&providers, id))
                                    .cloned();
                                if let Some(provider) = provider {
                                    for model in &mut models {
                                        crate::catalog::enrich(&provider, model);
                                    }
                                }
                                let _ = ctx.event_tx
                                    .send(Event::ModelsLoaded {
                                        provider_name,
                                        models,
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::ModelsError {
                                        provider_name,
                                        error: e.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::AddProvider { id, config: pc } => {
                        ctx.connections.providers.insert(id.clone(), pc);
                        ctx.clients.remove(&id);
                        persist(
                            &ctx.config,
                            &ctx.connections,
                            config_path.as_deref(),
                            connections_path.as_deref(),
                            &ctx.event_tx,
                        )
                        .await;
                    }
                    Command::AuthProviderLogin { name } => {
                        handle_auth_provider_login(
                            name,
                            &ctx.connections.providers,
                            &ctx.clients,
                            &ctx.event_tx,
                        )
                        .await;
                    }
                    Command::RemoveProvider { name } => {
                        ctx.connections.providers.remove(&name);
                        ctx.clients.remove(&name);
                        if ctx.connections.active.as_ref().map(|a| a.provider.as_str())
                            == Some(name.as_str())
                        {
                            ctx.connections.active = None;
                        }
                        persist(
                            &ctx.config,
                            &ctx.connections,
                            config_path.as_deref(),
                            connections_path.as_deref(),
                            &ctx.event_tx,
                        )
                        .await;
                    }
                    Command::SetActiveProvider { name } => {
                        if ctx.connections.providers.contains_key(&name) {
                            let active = ctx.connections.active.get_or_insert_with(|| {
                                shuvarie_config::Active {
                                    provider: name.clone(),
                                    model: None,
                                    variant: None,
                                }
                            });
                            active.provider = name.clone();
                            if !ctx.clients.contains_key(&name)
                                && let Some(pc) = ctx.connections.providers.get(&name)
                                && let Ok(client) = build_client(pc, &ctx.event_tx)
                            {
                                ctx.clients.insert(name.clone(), client);
                            }
                            persist(
                                &ctx.config,
                                &ctx.connections,
                                config_path.as_deref(),
                                connections_path.as_deref(),
                                &ctx.event_tx,
                            )
                            .await;
                        }
                    }
                    Command::SetActiveModel { model } => {
                        if let Some(active) = ctx.connections.active.as_mut() {
                            active.model = Some(model);
                            active.variant = None;
                        }
                        persist(
                            &ctx.config,
                            &ctx.connections,
                            config_path.as_deref(),
                            connections_path.as_deref(),
                            &ctx.event_tx,
                        )
                        .await;
                    }
                    Command::CycleVariant => {
                        let (provider, model, current) = match ctx.connections.active.as_ref() {
                            Some(active) => (
                                active.provider.clone(),
                                active.model.clone(),
                                active.variant.clone(),
                            ),
                            None => continue,
                        };
                        let next = ctx
                            .connections
                            .providers
                            .get(&provider)
                            .and_then(|pc| {
                                model.as_deref().and_then(|model| {
                                    crate::catalog::next_variant(pc, model, current.as_deref())
                                })
                            });
                        if let Some(next) = next {
                            if let Some(active) = ctx.connections.active.as_mut() {
                                active.variant = match next {
                                    crate::catalog::NextVariant::Set(variant) => Some(variant),
                                    crate::catalog::NextVariant::Default => None,
                                };
                            }
                            persist(
                                &ctx.config,
                                &ctx.connections,
                                config_path.as_deref(),
                                connections_path.as_deref(),
                                &ctx.event_tx,
                            )
                            .await;
                        }
                    }
                    Command::SelectVariant { variant } => {
                        let Some(active) = ctx.connections.active.as_mut() else {
                            continue;
                        };
                        active.variant = variant;
                        persist(
                            &ctx.config,
                            &ctx.connections,
                            config_path.as_deref(),
                            connections_path.as_deref(),
                            &ctx.event_tx,
                        )
                        .await;
                    }
                    Command::SaveConfig => {
                        persist(
                            &ctx.config,
                            &ctx.connections,
                            config_path.as_deref(),
                            connections_path.as_deref(),
                            &ctx.event_tx,
                        )
                        .await;
                    }
                    Command::NewSession => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        overflow_retries = 0;
                        pending_retry = None;
                        conn_retries = 0;
                        ctx.context_announced = false;
                        release_active_lock(&mut ctx).await;
                        clear_steered(&mut steered, &ctx.steer, &ctx.event_tx).await;
                        dismiss_pending_questions(&mut pending_questions);
                        dismiss_pending_permissions(&mut pending_permissions);
                        ctx.session = Some(Arc::new(Mutex::new(Session::new())));
                        let _ = ctx.event_tx.send(Event::SessionStarted).await;
                    }
                    Command::SendMessage { content, model } => {
                        if is_busy(&ctx, pending_retry.as_ref()) {
                            steered.push((content.clone(), model));
                            ctx.steer.arm();
                            let _ = ctx.event_tx.send(Event::PromptSteered { content }).await;
                            continue;
                        }
                        overflow_retries = 0;
                        pending_retry = None;
                        conn_retries = 0;
                        ctx.active_stream = None;
                        ctx.start_user_turn(content, false, model).await;
                    }
                    Command::RunBash { command } => {
                        let id = next_bash_id;
                        next_bash_id += 1;
                        let _ = ctx
                            .event_tx
                            .send(Event::BashStarted {
                                id,
                                command: command.clone(),
                            })
                            .await;
                        let shell = ctx.shell.clone();
                        let event_tx = ctx.event_tx.clone();
                        let cwd = ctx.workspace_root.clone();
                        tokio::spawn(async move {
                            let started = std::time::Instant::now();
                            let (chunk_tx, mut chunk_rx) =
                                tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(64);
                            let pump = tokio::spawn({
                                let event_tx = event_tx.clone();
                                async move {
                                    while let Some(chunk) = chunk_rx.recv().await {
                                        let _ = event_tx
                                            .send(Event::BashOutput {
                                                id,
                                                stdout: chunk.stdout,
                                                stderr: chunk.stderr,
                                            })
                                            .await;
                                    }
                                }
                            });
                            let result = crate::tools::run_shell_command(
                                &shell,
                                &command,
                                &cwd,
                                None,
                                &crate::tools::ShellOutputTx::full(chunk_tx),
                                None,
                            )
                            .await;
                            let _ = pump.await;
                            let duration_ms = started.elapsed().as_millis() as u64;
                            let finished = match result {
                                Ok(run) => Event::BashFinished {
                                    id,
                                    ok: run.status.success(),
                                    exit: run.status.code(),
                                    stdout: run.out,
                                    stderr: run.err,
                                    duration_ms,
                                },
                                Err(error) => Event::BashFinished {
                                    id,
                                    ok: false,
                                    exit: None,
                                    stdout: error,
                                    stderr: String::new(),
                                    duration_ms,
                                },
                            };
                            let _ = event_tx.send(finished).await;
                        });
                    }
                    Command::CancelStream => {
                        let mut aborted = cut_running_stream(
                            &mut ctx,
                            &mut pending_questions,
                            &mut pending_permissions,
                        )
                        .await;
                        if !aborted
                            && let Some(pending) = pending_retry.take()
                        {
                            conn_retries = 0;
                            let _ = ctx.event_tx.send(Event::StreamError { error: pending.message }).await;
                            aborted = true;
                        }
                        // The current agent-loop window ended; the next
                        // steered prompt takes over immediately.
                        if aborted {
                            ctx.steer.reset();
                            if !steered.is_empty() {
                                let (content, model) = steered.remove(0);
                                ctx.active_stream = None;
                                ctx.start_user_turn(content, true, model).await;
                            }
                        }
                    }
                    Command::ListSessions => match ctx.store.list_sessions().await {
                        Ok(sessions) => {
                            let _ = ctx.event_tx.send(Event::SessionsLoaded { sessions }).await;
                        }
                        Err(e) => {
                            let _ = ctx.event_tx
                                .send(Event::SessionError {
                                    error: e.to_string(),
                                })
                                .await;
                        }
                    },
                    Command::LoadSession { id } => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        match ctx.store.acquire_session_lock(id, now_ms()).await {
                            Ok(LockAcquire::Held) => {
                                let _ = ctx.event_tx.send(Event::SessionLocked { id }).await;
                                continue;
                            }
                            Ok(_) => {
                                if ctx.locked_session != Some(id) {
                                    release_active_lock(&mut ctx).await;
                                    ctx.locked_session = Some(id);
                                }
                            }
                            Err(_) => {}
                        }
                        clear_steered(&mut steered, &ctx.steer, &ctx.event_tx).await;
                        dismiss_pending_questions(&mut pending_questions);
                        dismiss_pending_permissions(&mut pending_permissions);
                        match ctx.store.load_session(id).await {
                            Ok(stored) => {
                                let loaded = Session::from_stored(stored);
                                ctx.session = Some(Arc::new(Mutex::new(loaded.clone())));
                                ctx.context_announced = false;
                                let _ = ctx.event_tx
                                    .send(Event::SessionLoaded {
                                        id,
                                        title: loaded.title.clone().unwrap_or_default(),
                                        session: loaded,
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: e.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::SaveScroll { id, scroll } => {
                        // Fire-and-forget: a failed scroll write is harmless
                        // and must never surface as a session error.
                        let _ = ctx.store.set_scroll(id, scroll).await;
                    }
                    Command::SetTitle { title } => {
                        let title = title.trim();
                        if title.is_empty() {
                            continue;
                        }
                        let Some(s) = &ctx.session else { continue; };
                        let mut guard = s.lock().await;
                        let Some(id) = guard.id else { continue; };
                        if guard.title.as_deref() == Some(title) {
                            continue;
                        }
                        match ctx.store.set_title(id, title).await {
                            Ok(()) => {
                                guard.title = Some(title.to_string());
                                drop(guard);
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionTitleChanged {
                                        id,
                                        title: title.to_string(),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                drop(guard);
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: format!("failed to set title: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::GenTitle => {
                        let Some(s) = &ctx.session else {
                            let _ = ctx
                                .event_tx
                                .send(Event::SessionError {
                                    error: "no active session".to_string(),
                                })
                                .await;
                            continue;
                        };
                        let request = {
                            let guard = s.lock().await;
                            match (guard.id, guard.title.clone(), guard.first_user_prompt()) {
                                (Some(id), Some(title), Some(prompt)) => {
                                    Some((id, title, prompt.to_string()))
                                }
                                _ => None,
                            }
                        };
                        let Some((id, current, prompt)) = request else {
                            let _ = ctx
                                .event_tx
                                .send(Event::SessionError {
                                    error: "no user prompt yet".to_string(),
                                })
                                .await;
                            continue;
                        };
                        let title_cfg = ctx.config.ui.title.clone();
                        if let Err(error) = spawn_title_draft(
                            &mut ctx.clients,
                            &mut ctx.connections,
                            &ctx.event_tx,
                            &mut ctx.store,
                            s.clone(),
                            id,
                            &title_cfg,
                            // Compare-and-swap from the current title: a
                            // manual rename that lands while the call runs
                            // wins, and a concurrently finishing automatic
                            // draft (which swaps from the provisional
                            // title) cannot clobber the result.
                            current,
                            prompt,
                        ) {
                            // The manual trigger surfaces resolve/connection
                            // failures as errors instead of the automatic
                            // draft's silent no-op.
                            let _ = ctx
                                .event_tx
                                .send(Event::SessionError { error })
                                .await;
                        }
                    }
                    Command::DeleteSession { id } => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        if let Ok(true) = ctx.store.locked_by_other(id, now_ms()).await {
                            let _ = ctx.event_tx.send(Event::SessionLocked { id }).await;
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        clear_steered(&mut steered, &ctx.steer, &ctx.event_tx).await;
                        match ctx.store.delete_session(id).await {
                            Ok(()) => {
                                if ctx.locked_session == Some(id) {
                                    ctx.locked_session = None;
                                }
                                if let Some(s) = &ctx.session
                                    && s.lock().await.id == Some(id)
                                {
                                    *s.lock().await = Session::new();
                                    ctx.context_announced = false;
                                }
                                let _ = ctx.event_tx.send(Event::SessionDeleted { id }).await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: e.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::SearchHistory { query } => {
                        let query = query.trim().to_string();
                        if query.is_empty() {
                            let _ = ctx.event_tx.send(Event::SearchResults { hits: vec![] }).await;
                            continue;
                        }
                        let mut fts_hits = match ctx.store.search_messages(&query, SEARCH_LIMIT).await {
                            Ok(hits) => hits,
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::SearchError {
                                        error: e.to_string(),
                                    })
                                    .await;
                                continue;
                            }
                        };
                        if let Some(setup) = &ctx.embedding_setup {
                            if let Some(handle) = semantic_search.take() {
                                handle.abort();
                            }
                            let store_sem = ctx.store.clone();
                            let setup_sem = setup.clone();
                            let tx_sem = ctx.event_tx.clone();
                            let fts_sem = std::mem::take(&mut fts_hits);
                            let query_sem = query.clone();
                            semantic_search = Some(
                                tokio::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                                    let texts = vec![query_sem];
                                    let vecs = setup_sem
                                        .client
                                        .embed(&setup_sem.model, setup_sem.dims, &texts)
                                        .await;
                                    let Ok(mut vecs) = vecs else { return };
                                    let Some(vec) = vecs.pop() else { return };
                                    let Ok(semantic_hits) =
                                        store_sem.clone().semantic_search(vec, SEARCH_LIMIT).await
                                    else {
                                        return;
                                    };
                                    let merged = embeddings::rrf_merge(
                                        fts_sem,
                                        semantic_hits,
                                        60,
                                        SEARCH_LIMIT as usize,
                                    );
                                    let _ = tx_sem.send(Event::SearchResults { hits: merged }).await;
                                })
                                .abort_handle(),
                            );
                        }
                        let _ = ctx.event_tx.send(Event::SearchResults { hits: fts_hits }).await;
                    }
                    Command::AnswerQuestion { id, answers } => {
                        if let Some(respond) = pending_questions.remove(&id) {
                            let _ = respond.send(answers);
                        }
                    }
                    Command::PermissionDecide { id, decision } => {
                        if let Some(respond) = pending_permissions.remove(&id) {
                            let _ = respond.send(decision);
                        }
                    }
                    Command::ForkSession { node, summarize } => {
                        // A fork rewinds the active path, so a running turn is
                        // cut first (persisted interrupted) and the queued
                        // steered prompts belong to a discarded context: wipe
                        // the queue and let the TUI drop its display.
                        if cut_running_stream(
                            &mut ctx,
                            &mut pending_questions,
                            &mut pending_permissions,
                        )
                        .await
                        {
                            ctx.steer.reset();
                        }
                        clear_steered(&mut steered, &ctx.steer, &ctx.event_tx).await;
                        pending_retry = None;
                        conn_retries = 0;
                        let Some(s) = &ctx.session else { continue; };
                        let Some(sid) = s.lock().await.id else { continue; };
                        let summarizer = if summarize {
                            let Some(active) = ctx.connections.active.clone() else {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError {
                                        error: "no active provider to summarize".into(),
                                    })
                                    .await;
                                continue;
                            };
                            let Some(model) = active.model.clone() else {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError {
                                        error: "no active model to summarize".into(),
                                    })
                                    .await;
                                continue;
                            };
                            match client_for(
                                &mut ctx.clients,
                                &mut ctx.connections,
                                &ctx.event_tx,
                                &active.provider,
                            ) {
                                Ok(c) => Some((c.clone(), model)),
                                Err(e) => {
                                    let _ = ctx
                                        .event_tx
                                        .send(Event::SessionError { error: e })
                                        .await;
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        match fork_session(
                            &mut ctx.store,
                            sid,
                            node,
                            summarize,
                            summarizer.as_ref(),
                            &ctx.event_tx,
                        )
                        .await
                        {
                            Ok(prompt) => {
                                if let Err(e) = reload_and_emit(&mut ctx, sid, prompt).await {
                                    let _ = ctx
                                        .event_tx
                                        .send(Event::SessionError { error: e })
                                        .await;
                                }
                            }
                            Err(e) => {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError {
                                        error: format!("fork failed: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::DeleteBranch { node } => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        let Some(s) = &ctx.session else { continue; };
                        let Some(sid) = s.lock().await.id else { continue; };
                        match ctx.store.load_session(sid).await {
                            Ok(stored) => {
                                if subtree_contains(&stored, node, stored.leaf_id) {
                                    let _ = ctx
                                        .event_tx
                                        .send(Event::SessionError {
                                            error: "cannot delete the active branch".into(),
                                        })
                                        .await;
                                    continue;
                                }
                                if let Err(e) = ctx.store.delete_branch(sid, node).await {
                                    let _ = ctx
                                        .event_tx
                                        .send(Event::SessionError {
                                            error: format!("branch delete failed: {e}"),
                                        })
                                        .await;
                                    continue;
                                }
                                match ctx.store.load_session(sid).await {
                                    Ok(stored) => {
                                        let _ = ctx
                                            .event_tx
                                            .send(Event::SessionTree {
                                                session: Session::from_stored(stored),
                                            })
                                            .await;
                                    }
                                    Err(e) => {
                                        let _ = ctx
                                            .event_tx
                                            .send(Event::SessionError {
                                                error: e.to_string(),
                                            })
                                            .await;
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError { error: e.to_string() })
                                    .await;
                            }
                        }
                    }
                    Command::OpenTree => {
                        let Some(s) = &ctx.session else { continue; };
                        let Some(sid) = s.lock().await.id else { continue; };
                        match ctx.store.load_session(sid).await {
                            Ok(stored) => {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionTree {
                                        session: Session::from_stored(stored),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError { error: e.to_string() })
                                    .await;
                            }
                        }
                    }
                    Command::ExportSession { path } => {
                        let Some(s) = &ctx.session else { continue; };
                        let Some(sid) = s.lock().await.id else { continue; };
                        match export_session(&mut ctx.store, sid, path.as_deref()).await {
                            Ok(path) => {
                                let _ = ctx.event_tx.send(Event::SessionExported { path }).await;
                            }
                            Err(e) => {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError { error: e.to_string() })
                                    .await;
                            }
                        }
                    }
                    Command::SwitchScene { name } => {
                        if let Some(error) = switch_scene(&mut ctx, pending_retry.as_ref(), name)
                            .await
                        {
                            let _ = ctx.event_tx.send(Event::SceneError { error }).await;
                        }
                    }
                    Command::Replay => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        ctx.steer.reset();
                        let Some(s) = &ctx.session else { continue; };
                        let session_id = s.lock().await.id;
                        let Some(sid) = session_id else { continue; };
                        let last_user_content = s.lock().await.last_user_node()
                            .map(|n| n.content.clone());
                        match fork_session(&mut ctx.store, sid, None, false, None, &ctx.event_tx)
                            .await
                        {
                            Ok(_) => {
                                if let Err(e) =
                                    reload_and_emit(&mut ctx, sid, None).await
                                {
                                    let _ = ctx
                                        .event_tx
                                        .send(Event::SessionError { error: e })
                                        .await;
                                    continue;
                                }
                                if let Some(content) = last_user_content {
                                    ctx.self_replay_send(content, true, None).await;
                                }
                            }
                            Err(e) => {
                                let _ = ctx
                                    .event_tx
                                    .send(Event::SessionError {
                                        error: format!("replay failed: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::Reload => {
                        ctx.skills = crate::skills::Skills::load(
                            &ctx.workspace_root,
                            &ctx.config.skills,
                            &ctx.trust,
                        );
                        let _ = ctx
                            .event_tx
                            .send(Event::SkillsLoaded {
                                skills: ctx.skills.skills.clone(),
                                warnings: ctx.skills.warnings.clone(),
                            })
                            .await;
                        let custom_commands =
                            crate::custom_commands::CustomCommands::load(&ctx.workspace_root);
                        let _ = ctx
                            .event_tx
                            .send(Event::CustomCommandsLoaded {
                                commands: custom_commands.commands.clone(),
                                warnings: custom_commands.warnings.clone(),
                            })
                            .await;
                    }
                    Command::RecallSteered { stacked } => {
                        let content = steered.pop().map(|(content, _)| content);
                        if steered.is_empty() {
                            ctx.steer.disarm();
                        }
                        let _ = ctx
                            .event_tx
                            .send(Event::SteeredRecalled { stacked, content })
                            .await;
                    }
                    Command::LspStart { name } => {
                        let mut mgr = ctx.lsp.lock().await;
                        match mgr.start(&name).await {
                            Ok(()) => {
                                emit_lsp_status(&mgr, &ctx.event_tx).await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx.send(Event::LspError { error: e }).await;
                            }
                        }
                    }
                    Command::LspStop { name } => {
                        let mut mgr = ctx.lsp.lock().await;
                        if let Err(e) = mgr.stop(&name).await {
                            let _ = ctx.event_tx.send(Event::LspError { error: e }).await;
                        }
                        emit_lsp_status(&mgr, &ctx.event_tx).await;
                    }
                    Command::LspRestart { name } => {
                        let mut mgr = ctx.lsp.lock().await;
                        match mgr.restart(&name).await {
                            Ok(()) => {
                                emit_lsp_status(&mgr, &ctx.event_tx).await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx.send(Event::LspError { error: e }).await;
                            }
                        }
                    }
                    Command::LspList { all, filter } => {
                        let mgr = ctx.lsp.lock().await;
                        let entries = mgr.list(all, filter.as_deref());
                        let mut text = String::new();
                        if entries.is_empty() {
                            text.push_str("no servers matching filter");
                        } else {
                            for e in entries {
                                text.push_str(&format!(
                                    "{:<12} {:<28} {}\n",
                                    e.name,
                                    e.command.join(" "),
                                    e.status.as_str()
                                ));
                            }
                        }
                        let _ = ctx.event_tx
                            .send(Event::LspStatus {
                                servers: mgr.status_snapshot(),
                            })
                            .await;
                        // The list text is surfaced via the side channel of the `lsp` tool itself,
                        // not as a dedicated event; the tool returns it directly.
                        let _ = text;
                    }
                    Command::McpList => {
                        let mgr = ctx.mcp.lock().await;
                        emit_mcp_status(&mgr, &ctx.event_tx).await;
                    }
                    Command::McpReconnect { name } => {
                        let mut mgr = ctx.mcp.lock().await;
                        match mgr.reconnect(&name).await {
                            Ok(()) => emit_mcp_status(&mgr, &ctx.event_tx).await,
                            Err(error) => {
                                // The error notice plus a snapshot in which
                                // the server carries its failure detail.
                                let _ = ctx.event_tx.send(Event::McpError { error }).await;
                                emit_mcp_status(&mgr, &ctx.event_tx).await;
                            }
                        }
                    }
                }
            }
            _ = lsp_pump_tick.tick(), if ctx.lsp.try_lock().map(|m| m.has_active_servers()).unwrap_or(false) => {
                let mut mgr = ctx.lsp.lock().await;
                let updates = mgr.pump_diagnostics().await;
                let had_updates = !updates.is_empty();
                for upd in updates {
                    let _ = ctx.event_tx
                        .send(Event::LspDiagnostics {
                            path: upd.path,
                            diagnostics: upd.diagnostics,
                        })
                        .await;
                }
                if had_updates {
                    emit_lsp_status(&mgr, &ctx.event_tx).await;
                }
            }
            _ = lock_beat.tick() => {
                if let Some(id) = ctx.locked_session {
                    match ctx.store.touch_session_lock(id, now_ms()).await {
                        Ok(true) => {}
                        Ok(false) => {
                            ctx.locked_session = None;
                            let _ = ctx.event_tx.send(Event::SessionLocked { id }).await;
                        }
                        Err(_) => {}
                    }
                }
            }
            question = question_rx.recv() => {
                let Some(req) = question else { break };
                let id = next_question_id;
                next_question_id = next_question_id.wrapping_add(1);
                pending_questions.insert(id, req.respond);
                let _ = ctx.event_tx
                    .send(Event::QuestionAsked {
                        id,
                        questions: req.questions,
                    })
                    .await;
            }
            permission = permission_rx.recv() => {
                let Some(req) = permission else { break };
                let id = next_permission_id;
                next_permission_id = next_permission_id.wrapping_add(1);
                pending_permissions.insert(id, req.respond);
                let _ = ctx.event_tx
                    .send(Event::PermissionRequested {
                        id,
                        description: req.description,
                        allow_session: req.scope.is_some(),
                    })
                    .await;
            }
            outcome = stream_done_rx.recv() => {
                let Some(outcome) = outcome else { continue };
                match outcome {
                    StreamOutcome::Finished | StreamOutcome::Preempted => {
                        overflow_retries = 0;
                        conn_retries = 0;
                        ctx.active_stream = None;
                        // A permission denial cut may leave sibling ask
                        // responders behind; drop them so late decisions are
                        // no-ops (a deny cut fires while tool calls are
                        // running, unlike a steer cut).
                        dismiss_pending_questions(&mut pending_questions);
                        dismiss_pending_permissions(&mut pending_permissions);
                        // Steer in the next queued prompt, if any. The queue
                        // keeps preempting: the fresh turn is armed again so
                        // the next queued prompt cuts in at its next action
                        // boundary instead of waiting for the whole turn.
                        if !steered.is_empty() {
                            let (content, model) = steered.remove(0);
                            ctx.start_user_turn(content, true, model).await;
                            if !steered.is_empty() {
                                ctx.steer.arm();
                            }
                        } else {
                            ctx.steer.reset();
                        }
                    }
                    StreamOutcome::Overflowed { compacted } => {
                        if compacted && overflow_retries < MAX_OVERFLOW_RETRIES {
                            overflow_retries += 1;
                            ctx.resume_last_turn().await;
                        } else {
                            overflow_retries = 0;
                            ctx.steer.reset();
                            let _ = ctx.event_tx
                                .send(Event::StreamError {
                                    error: "context budget exceeded and compaction could not keep up; \
                                            start a new message to continue"
                                        .into(),
                                })
                                .await;
                        }
                    }
                    StreamOutcome::ConnectionLost { reason, message } => {
                        match next_connection_retry(conn_retries, ctx.config.retry.max_retries) {
                            Some((attempt, delay_secs)) => {
                                conn_retries = attempt;
                                let _ = ctx
                                    .event_tx
                                    .send(Event::RetryScheduled {
                                        reason,
                                        message: message.clone(),
                                        attempt,
                                        max_attempts: ctx.config.retry.max_retries,
                                        delay_ms: delay_secs * 1000,
                                    })
                                    .await;
                                pending_retry = Some(PendingRetry {
                                    deadline: tokio::time::Instant::now()
                                        + std::time::Duration::from_secs(delay_secs),
                                    message,
                                });
                            }
                            None => {
                                conn_retries = 0;
                                ctx.steer.reset();
                                let _ = ctx
                                    .event_tx
                                    .send(Event::StreamError { error: message })
                                    .await;
                            }
                        }
                    }
                }
            }
            _ = async {
                if let Some(deadline) = retry_deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if retry_deadline.is_some() => {
                pending_retry = None;
                ctx.resume_last_turn().await;
            }
        }
    }

    release_active_lock(&mut ctx).await;

    ctx.lsp.lock().await.shutdown_all().await;
    ctx.mcp.lock().await.shutdown_all();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn release_active_lock(ctx: &mut CoreCtx) {
    if let Some(id) = ctx.locked_session.take() {
        let _ = ctx.store.release_session_lock(id).await;
    }
}

const SEARCH_LIMIT: u64 = 50;

async fn emit_lsp_status(mgr: &shuvarie_lsp::LspManager, event_tx: &Sender<Event>) {
    let _ = event_tx
        .send(Event::LspStatus {
            servers: mgr.status_snapshot(),
        })
        .await;
}

async fn emit_mcp_status(mgr: &shuvarie_mcp::McpManager, event_tx: &Sender<Event>) {
    let _ = event_tx
        .send(Event::McpStatus {
            servers: mgr.status_snapshot(),
        })
        .await;
}

const AGENT_PREAMBLE: &str = "\
You are Shuvarie, an agentic coding assistant running in a terminal inside the user's project. \
You can read, write, edit, and delete files, list directories, grep for text, run commands, and fetch \
web pages with the `webfetch` tool (URLs must start with http:// or https://). \
For advanced editing — changes spanning multiple files, renames/moves, or adding files — prefer \
the `apply_patch` tool: it applies one `*** Begin Patch` … `*** End Patch` envelope touching \
several files in a single call. Delete individual files with the `delete_file` tool. \
Prefer using tools to inspect the workspace and verify your work (for example, run the test \
suite after editing code) instead of guessing. When a tool reports an error, fix the cause and \
retry rather than stopping. After finishing the work, summarize what you did and any results in \
a short reply. Keep the reply concise.

You can also delegate work to three specialist worker agents, exposed as tools:
- explore_workspace: locates, reads, and summarizes existing code (list/read/grep). Use it for \
  research and understanding before changes.
- run_tests: runs the project's build, test, and lint commands and iterates on failures. Use it \
  to verify changes or diagnose failing commands.
- edit_files: implements changes by reading, writing, and editing files.

Delegate a task to a worker when it is long, multi-step, or self-contained — the worker runs its \
  own agent loop and returns a summary. Keep doing your own work for quick, single tool calls. \
You remain responsible for the final answer: synthesize worker results and verify the overall \
outcome (for example, delegate to run_tests after edit_files).

You can maintain a session todo list with the `todo` tool (ops: add, update, remove, list; \
statuses: pending, in_progress, done). When the user's request needs three or more steps, add \
a todo per step, keep exactly one in_progress while you work on it, and mark each done as soon \
as it is finished. Todos are surfaced to the user in the chat pane and the sidebar, so keep \
the list current.";

#[derive(Debug, Default)]
struct TurnState {
    assistant_message_id: Option<u64>,
    assistant_seq: u64,
    text_segments: Vec<shuvarie_db::TextSegment>,
    pending_reasoning: Vec<shuvarie_db::ReasoningSegment>,
    tool_records: Vec<crate::tool_record::ToolRecord>,
    /// Tool calls started but not yet finished, in start order: when the turn
    /// is cut they persist as killed records so a reload keeps their blocks.
    pending_tools: Vec<PendingToolCall>,
}

/// A tool call whose result has not arrived yet: the serialized args plus the
/// moment the call started, so the finished call can record its duration. The
/// name and owning worker also let a streamed shell chunk resolve back to its
/// own call.
struct PendingTool {
    name: String,
    worker: Option<String>,
    args_json: String,
    started: std::time::Instant,
}

/// A started tool call tracked in [`TurnState`] so an interrupted turn can
/// persist the calls that never returned as killed records.
#[derive(Debug, Clone)]
struct PendingToolCall {
    call_id: String,
    name: String,
    args_json: String,
    worker: Option<String>,
    started: std::time::Instant,
}

/// The provisional title for a new session: a trimmed prefix of the first
/// user prompt, capped at `max_chars` (the `ui.title` `max-chars` property),
/// or "Untitled session" for an empty prompt.
fn title_for(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        "Untitled session".to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

/// Spawn the fire-and-forget LLM title draft for session `id`: ask the
/// model configured under `ui.title` `llm` (defaults: the active provider's
/// catalog small model) to title the session from `prompt`, then
/// compare-and-swap the store title from `expected` to the sanitized reply —
/// the write lands only while the title still reads `expected`, so a manual
/// rename that lands meanwhile wins and two racing drafts cannot clobber
/// each other. The in-memory session and a `SessionTitleChanged` event are
/// updated only when the swap took effect. `Err(reason)` when no model
/// resolves or the provider client cannot be built — the automatic draft
/// ignores it, the manual `gen-title` command reports it.
#[allow(clippy::too_many_arguments)]
fn spawn_title_draft(
    clients: &mut HashMap<String, ProviderClient>,
    connections: &mut Connections,
    event_tx: &Sender<Event>,
    store: &mut Store,
    session: Arc<Mutex<Session>>,
    id: uuid::Uuid,
    cfg: &TitleConfig,
    expected: String,
    prompt: String,
) -> Result<(), String> {
    let Some((name, model)) =
        crate::title::resolve_model(connections, cfg.provider.as_deref(), cfg.model.as_deref())
    else {
        return Err("no model available for title generation".to_string());
    };
    let client = client_for(clients, connections, event_tx, &name)
        .cloned()
        .map_err(|e| format!("title generation failed: {e}"))?;
    let mut store = store.clone();
    let event_tx = event_tx.clone();
    let preamble = cfg.system_prompt.clone();
    tokio::spawn(async move {
        let Some(generated) =
            crate::title::generate(&client, &model, preamble.as_deref(), &prompt).await
        else {
            return;
        };
        if let Ok(true) = store.set_title_if(id, &expected, &generated).await {
            // Only when the in-memory title is still the expected one: a
            // manual rename that landed after the swap wins instead.
            let mut guard = session.lock().await;
            if guard.title.as_deref() == Some(expected.as_str()) {
                guard.title = Some(generated.clone());
                drop(guard);
                let _ = event_tx
                    .send(Event::SessionTitleChanged {
                        id,
                        title: generated,
                    })
                    .await;
            }
        }
    });
    Ok(())
}

/// The active path's message ids, root → tip: the parent chain from the
/// stored leaf, with [`shuvarie_db::EMPTY_LEAF`] meaning a cleared path (an
/// empty chain) and the newest message as a fallback for legacy unset or
/// dangling leaves. The `seen` guard makes a corrupt parent cycle terminate.
fn chain_of(stored: &shuvarie_db::StoredSession) -> Vec<u64> {
    let by_id: std::collections::HashMap<u64, Option<u64>> = stored
        .messages
        .iter()
        .map(|m| (m.id, m.parent_id))
        .collect();
    let newest = || stored.messages.iter().max_by_key(|m| m.seq).map(|m| m.id);
    let mut leaf = match stored.leaf_id {
        Some(shuvarie_db::EMPTY_LEAF) => None,
        Some(id) => by_id.contains_key(&id).then_some(id).or_else(newest),
        None => newest(),
    };
    let mut ids = Vec::with_capacity(stored.messages.len());
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = leaf {
        if !seen.insert(id) {
            break;
        }
        ids.push(id);
        leaf = by_id.get(&id).copied().flatten();
    }
    ids.reverse();
    ids
}

/// Whether `needle` (usually the active leaf) lies in the subtree rooted at
/// `root_id`.
fn subtree_contains(
    stored: &shuvarie_db::StoredSession,
    root_id: u64,
    needle: Option<u64>,
) -> bool {
    let Some(needle) = needle else {
        return false;
    };
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for m in &stored.messages {
        if let Some(parent) = m.parent_id {
            children.entry(parent).or_default().push(m.id);
        }
    }
    let mut queue = vec![root_id];
    while let Some(id) = queue.pop() {
        if id == needle {
            return true;
        }
        if let Some(kids) = children.remove(&id) {
            queue.extend(kids);
        }
    }
    false
}

/// Fork the session at a node: the active path is re-rooted *before* the
/// node — its parent becomes the fork tip and the node's own content is
/// returned for recall into the input — optionally creating an LLM summary
/// of the prefix before the fork point first. Summary and system nodes are
/// compaction markers rather than turns, so they walk to themselves without
/// a recall. `node: None` walks to the active path's last user prompt
/// (`/undo`). Returns the recalled content, `None` when there was nothing
/// to fork.
async fn fork_session(
    store: &mut Store,
    session_id: uuid::Uuid,
    node_id: Option<u64>,
    summarize: bool,
    summarizer: Option<&(ProviderClient, String)>,
    event_tx: &Sender<Event>,
) -> Result<Option<String>, String> {
    let stored = store
        .load_session(session_id)
        .await
        .map_err(|e| e.to_string())?;
    let by_id: HashMap<u64, &shuvarie_db::StoredMessage> =
        stored.messages.iter().map(|m| (m.id, m)).collect();
    let target = match node_id {
        Some(id) => Some(id),
        None => {
            let chain = chain_of(&stored);
            chain
                .iter()
                .rev()
                .filter_map(|id| by_id.get(id).copied())
                .find(|m| m.role == shuvarie_db::MsgRole::User)
                .map(|m| m.id)
        }
    };
    let Some(target) = target else {
        return Ok(None);
    };
    let node = by_id.get(&target).ok_or("unknown node")?;
    let (fork_tip, prompt): (Option<u64>, Option<String>) =
        if node.summary || node.role == shuvarie_db::MsgRole::System {
            (Some(node.id), None)
        } else {
            let recalled = (!node.content.trim().is_empty()).then(|| node.content.clone());
            (node.parent_id, recalled)
        };

    if summarize {
        let Some(tip) = fork_tip else {
            // Forking before the root prompt: nothing to summarize, the tree
            // simply walks to an empty path.
            store
                .set_active_leaf(session_id, None)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(prompt);
        };
        // The prefix to summarize is the fork tip's own ancestor chain
        // (root → tip), which works for nodes off the current path too.
        let tip_chain: Vec<u64> = {
            let mut ids = Vec::new();
            let mut cur = Some(tip);
            let mut seen = std::collections::HashSet::new();
            while let Some(id) = cur {
                if !seen.insert(id) {
                    break;
                }
                ids.push(id);
                cur = by_id.get(&id).and_then(|m| m.parent_id);
            }
            ids.reverse();
            ids
        };
        let prefix: Vec<shuvarie_db::StoredMessage> = tip_chain
            .iter()
            .filter_map(|id| by_id.get(id).map(|m| (*m).clone()))
            .collect();
        let chain_index: HashMap<u64, usize> = tip_chain
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();
        let records = span_tool_records(&stored, &chain_index, 0..prefix.len());
        let head_text = crate::compaction::serialize_head(&prefix, &records, 0);
        let Some((client, model)) = summarizer else {
            return Err("summarization requested without an active provider".into());
        };
        let _ = event_tx.send(Event::CompactionStarted).await;
        let summary = crate::compaction::summarize(client, model, &head_text).await;
        let _ = event_tx.send(Event::CompactionFinished).await;
        let summary = summary.map_err(|e| format!("summarization failed: {e}"))?;
        // Turn forks: the summary hangs from the fork tip — where the
        // selected node hung — and the forked-away node is reparented under
        // it. Marker forks walk to the marker itself: the new summary takes
        // its place in the chain.
        let marker = node.summary || node.role == shuvarie_db::MsgRole::System;
        let (parent, reparent) = if marker {
            (node.parent_id, None)
        } else {
            (Some(tip), Some(target))
        };
        let summary_msg = store
            .append_summary(session_id, parent, &summary)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(id) = reparent {
            store
                .set_message_parent(id, Some(summary_msg.id))
                .await
                .map_err(|e| e.to_string())?;
        }
    } else {
        store
            .set_active_leaf(session_id, fork_tip)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(prompt)
}

/// Reload a session after a fork and emit [`Event::Forked`], swapping the
/// in-memory session for the reloaded path.
async fn reload_and_emit(
    ctx: &mut CoreCtx,
    session_id: uuid::Uuid,
    prompt: Option<String>,
) -> Result<(), String> {
    let stored = ctx
        .store
        .load_session(session_id)
        .await
        .map_err(|e| e.to_string())?;
    let loaded = Session::from_stored(stored);
    if let Some(s) = &ctx.session {
        *s.lock().await = loaded.clone();
    }
    let _ = ctx
        .event_tx
        .send(Event::Forked {
            session: loaded,
            prompt,
        })
        .await;
    Ok(())
}

impl CoreCtx {
    /// Persist and start streaming a new user turn: an accepted `SendMessage`
    /// or a dispatched steered prompt. Emits [`Event::TurnStarted`] (with the
    /// `steered` flag) before the first stream event so the TUI renders the
    /// user prompt in order. `model` is the per-turn streaming override (see
    /// [`Command::SendMessage`]); `None` streams on the active provider.
    async fn start_user_turn(&mut self, content: String, steered: bool, model: Option<String>) {
        // A new turn always starts with the preemption signal off: dispatched
        // steered prompts only preempt the stream they were queued during,
        // and a fresh turn must not inherit a stale armed signal.
        self.steer.reset();
        if self.session.is_none() {
            self.session = Some(Arc::new(Mutex::new(Session::new())));
            let _ = self.event_tx.send(Event::SessionStarted).await;
        }
        let s = self.session.clone().unwrap();
        s.lock().await.push_user(content.clone());

        {
            let mut guard = s.lock().await;
            if guard.id.is_none() {
                let title_cfg = self.config.ui.title.clone();
                let title = title_for(&content, title_cfg.max_chars);
                let scene = guard.scene.take().or_else(|| self.scenes.default.clone());
                match self
                    .store
                    .create_session(
                        &title,
                        self.connections
                            .active
                            .as_ref()
                            .map(|a| a.provider.as_str()),
                        self.connections
                            .active
                            .as_ref()
                            .and_then(|a| a.model.as_deref()),
                        scene.as_deref(),
                    )
                    .await
                {
                    Ok(id) => {
                        guard.id = Some(id);
                        guard.title = Some(title.clone());
                        guard.scene = scene.clone();
                        // The row starts under this scene, so it is already
                        // announced: the first request injects no interlude
                        // for it.
                        guard.announced_scene = scene.clone();
                        match self.store.acquire_session_lock(id, now_ms()).await {
                            Ok(LockAcquire::Acquired | LockAcquire::Ours) => {
                                self.locked_session = Some(id);
                            }
                            Ok(LockAcquire::Held) | Err(_) => {}
                        }
                        let _ = self
                            .event_tx
                            .send(Event::SessionCreated {
                                id,
                                title: title.clone(),
                                scene,
                            })
                            .await;
                        // Draft the session title in the background when
                        // `ui.title` enables `auto-gen`: the provisional
                        // `title_for` heuristic stands until the generated
                        // title arrives, and the compare-and-swap write
                        // upgrades it only while no manual rename has
                        // landed meanwhile. Fire-and-forget — a failed or
                        // empty generation keeps the provisional title.
                        if title_cfg.auto_gen {
                            let _ = spawn_title_draft(
                                &mut self.clients,
                                &mut self.connections,
                                &self.event_tx,
                                &mut self.store,
                                s.clone(),
                                id,
                                &title_cfg,
                                title,
                                content.clone(),
                            );
                        }
                    }
                    Err(e) => {
                        let _ = self
                            .event_tx
                            .send(Event::StreamError {
                                error: format!("failed to create session: {e}"),
                            })
                            .await;
                        return;
                    }
                }
            }
            let id = guard.id.expect("cannot be poisoned");
            let parent = guard.leaf_id;
            let seq = guard.messages.len() - 1;
            let msg = self
                .store
                .append_message(id, parent, guard.messages.last().unwrap().role, &content)
                .await;
            match msg {
                Ok(msg) => {
                    guard.leaf_id = Some(msg.id);
                    if let Some(setup) = &self.embedding_setup {
                        let store_idx = self.store.clone();
                        let setup_idx = setup.clone();
                        let content_idx = msg.content.clone();
                        tokio::spawn(async move {
                            let _ = embeddings::index_message(
                                &mut store_idx.clone(),
                                &setup_idx,
                                msg.id,
                                id,
                                seq as u64,
                                &content_idx,
                            )
                            .await;
                        });
                    }
                }
                Err(e) => {
                    let _ = self
                        .event_tx
                        .send(Event::StreamError {
                            error: format!("failed to persist message: {e}"),
                        })
                        .await;
                    return;
                }
            }
        }

        let _ = self
            .event_tx
            .send(Event::TurnStarted {
                content: content.clone(),
                steered,
            })
            .await;
        self.self_replay_send(content, false, model).await;
    }

    /// Build a stream for the given user content and spawn the event-forwarding
    /// task. When `push_user` is set, the content is first appended as a user
    /// message (used by `SendMessage`); otherwise it is re-sent as-is (used by
    /// replay/resume). `model_override` is a per-turn `<provider_type>/<model>`
    /// spec (a custom command's `model` frontmatter); `None` streams on the
    /// active provider.
    async fn self_replay_send(
        &mut self,
        content: String,
        push_user: bool,
        model_override: Option<String>,
    ) {
        let Some(s) = &self.session else {
            return;
        };
        if push_user {
            let mut guard = s.lock().await;
            guard.push_user(content.clone());
            let id = guard.id;
            if let Some(id) = id {
                let parent = guard.leaf_id;
                if let Ok(msg) = self
                    .store
                    .append_message(
                        id,
                        parent,
                        guard.messages.last().expect("cannot be poisoned").role,
                        &content,
                    )
                    .await
                {
                    guard.leaf_id = Some(msg.id);
                }
            }
        }
        let (provider_name, model) = match model_override.as_deref() {
            Some(spec) => match resolve_model_override(
                &self.connections,
                &self.config.default_providers,
                spec,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    let _ = self.event_tx.send(Event::StreamError { error }).await;
                    return;
                }
            },
            None => {
                let Some(active) = self.connections.active.clone() else {
                    let _ = self
                        .event_tx
                        .send(Event::StreamError {
                            error: "no active provider".into(),
                        })
                        .await;
                    return;
                };
                let provider_name = active.provider;
                let Some(model) = active.model else {
                    let _ = self
                        .event_tx
                        .send(Event::StreamError {
                            error: "no active model".into(),
                        })
                        .await;
                    return;
                };
                (provider_name, model)
            }
        };
        let client = match client_for(
            &mut self.clients,
            &mut self.connections,
            &self.event_tx,
            &provider_name,
        ) {
            Ok(c) => c.clone(),
            Err(e) => {
                let _ = self.event_tx.send(Event::StreamError { error: e }).await;
                return;
            }
        };
        let (prior, todo_records, stored_scene, announced_scene, session_id) = {
            let guard = s.lock().await;
            (
                guard.history_for_send(),
                guard.tool_records.clone(),
                guard.scene.clone(),
                guard.announced_scene.clone(),
                guard.id,
            )
        };
        let scene = crate::scenes::Scene::resolve(&self.scenes, stored_scene.as_deref());
        // The scene announce: the current scene differs from the last
        // announced one only inside the deferral window after a mid-session
        // switch. The first request under the new scene injects the
        // interlude (what tells the model) and makes the switch durable;
        // updating the mirror keeps later requests interlude-free. A failed
        // persist leaves the mirror stale — the interlude still rides this
        // request, and the next one retries the write.
        let announce = stored_scene != announced_scene;
        if announce {
            match session_id {
                Some(id) => match self.store.set_scene(id, stored_scene.as_deref()).await {
                    Ok(()) => s.lock().await.announced_scene = stored_scene.clone(),
                    Err(e) => {
                        let _ = self
                            .event_tx
                            .send(Event::SceneError {
                                error: format!("failed to persist the scene: {e}"),
                            })
                            .await;
                    }
                },
                // No row yet (a pre-picked scene before its first turn):
                // the first turn's `create_session` records the scene, so
                // the mirror simply follows.
                None => s.lock().await.announced_scene = stored_scene.clone(),
            }
        }
        let todo_state = crate::tools::todos::TodoState::from_records(&todo_records);
        let agents_md = crate::context::load_agents_md(&self.workspace_root, &self.trust);
        let agents_budget = agents_md.remaining_budget();
        let loaded_context = agents_md.merged(crate::context::load_context_dir(
            &self.workspace_root,
            agents_budget,
            &self.trust,
        ));
        if !loaded_context.is_empty() && !self.context_announced {
            self.context_announced = true;
            let _ = self
                .event_tx
                .send(Event::ContextLoaded {
                    paths: loaded_context.files.clone(),
                })
                .await;
        }
        let base = match scene.prelude() {
            Some(prelude) => match self.skills.preamble_section() {
                Some(section) => format!("{prelude}\n\n{section}"),
                None => prelude.to_string(),
            },
            None => match self.skills.preamble_section() {
                Some(section) => format!("{AGENT_PREAMBLE}\n\n{section}"),
                None => AGENT_PREAMBLE.to_string(),
            },
        };
        let web_search_config = self.config.tools.effective_web_search();
        let web_search = web_search_config.as_ref();
        let base = if web_search.is_some() {
            format!(
                "{base}\n\nWeb search is available through the `web_search` tool: use it for \
                 current information and documentation beyond your training data, then read a \
                 specific result page with `webfetch`."
            )
        } else {
            base
        };
        let preamble = crate::context::build_preamble(&base, &loaded_context);
        let question_gate = QuestionGate::new(self.question_tx.clone());
        let (shell_tx, shell_rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(64);
        let file_locks = crate::tools::FileLocks::new();
        let tool_scene = scene.tools();
        // Connect the configured MCP servers before the roster is built so
        // their tools are offered this turn; a server that fails to connect
        // is skipped (surfacing of the failures arrives with the MCP
        // lifecycle events). Also snapshot the roster: the descriptor set is
        // read sync-side in `all_tools`.
        let (mcp_roster, _mcp_errors) = {
            let mut manager = self.mcp.lock().await;
            let mut errors = Vec::new();
            for name in manager.specs().keys().cloned().collect::<Vec<_>>() {
                if let Err(error) = manager.ensure_connected(&name).await {
                    errors.push(format!("MCP server '{name}': {error}"));
                }
            }
            // Surface the connect pass: connected servers (with their tool
            // counts) show in the sidebar, failures show their per-server
            // error line.
            let _ = self
                .event_tx
                .send(Event::McpStatus {
                    servers: manager.status_snapshot(),
                })
                .await;
            (manager.roster(), errors)
        };
        let tools = crate::tools::all_tools(
            self.lsp.clone(),
            file_locks.clone(),
            crate::tools::ReadCache::new(),
            self.max_output_chars,
            self.max_output_bytes,
            question_gate,
            self.access.clone(),
            crate::tools::ShellOutputTx::new(shell_tx.clone()),
            self.shell.clone(),
            todo_state,
            &tool_scene,
            web_search,
            &self.skills,
            &self.config.tools.tools,
            Some(&self.mcp),
            &mcp_roster,
        );
        let catalog_provider = crate::catalog::providers();
        let catalog_provider = self
            .connections
            .providers
            .get(&provider_name)
            .and_then(|pc| pc.catalog_id())
            .and_then(|id| crate::catalog::find_provider(&catalog_provider, id))
            .cloned();
        let budget = context_budget(&self.config, catalog_provider.as_ref(), &model)
            .map(|b| b.with_preamble_tokens(shuvarie_llm::estimate_text_tokens(&preamble)));
        let mut worker_set = crate::agents::build_workers(
            client.clone(),
            &model,
            self.lsp.clone(),
            file_locks,
            self.worker_turns,
            self.max_output_chars,
            self.max_output_bytes,
            budget.clone(),
            crate::tools::ShellOutputTx::new(shell_tx),
            self.shell.clone(),
            self.access.clone(),
            &scene,
            web_search,
            &self.skills,
        );
        let prior = crate::scenes::inject_history(&scene, &prior, Some(&content), announce);
        let stream = client
            .stream(
                &model,
                Some(&preamble),
                &content,
                &prior,
                tools,
                &mut worker_set.workers,
                self.manager_turns,
                budget,
            )
            .await;
        // Live `run_shell` output flows through the same merged turn stream as
        // the tool activity, so a chunk is always processed after its call's
        // `ToolStart` (earlier sub-streams poll first) and resolves to its own
        // call id in the stream loop.
        let stream = merge_shell_chunks(stream, shell_rx);
        let tx = self.event_tx.clone();
        let session_shared = s.clone();
        let client_shared = client.clone();
        let store_shared = self.store.clone();
        let model_shared = model.clone();
        let worker_usage = worker_set.usage;
        let keep_recent_tokens = self.config.context.keep_recent_tokens;
        let embedding_shared = self.embedding_setup.clone();
        let turn_state_shared = Arc::new(Mutex::new(TurnState::default()));
        let stream_done = self.stream_done_tx.clone();
        let steer_shared = self.steer.clone();
        let deny_cut_shared = self.access.turn_cut().clone();
        deny_cut_shared.reset();
        self.turn_state = Some(turn_state_shared.clone());
        self.active_stream = Some(
            tokio::spawn(async move {
                stream_stream_to_events(
                    stream,
                    session_shared,
                    client_shared,
                    catalog_provider,
                    store_shared,
                    model_shared,
                    keep_recent_tokens,
                    worker_usage,
                    embedding_shared,
                    tx,
                    turn_state_shared,
                    stream_done,
                    steer_shared,
                    deny_cut_shared,
                )
                .await;
            })
            .abort_handle(),
        );
    }

    /// Delete the interrupted assistant tip (message + tool calls), walk the
    /// leaf back to its parent (the user prompt), and re-stream from there.
    /// Used by the auto-continue path after a context overflow and the
    /// connection-retry resume. No undo-log entry — the retry rewrites the
    /// same branch position.
    async fn resume_last_turn(&mut self) {
        let Some(s) = &self.session else {
            return;
        };
        let Some(sid) = s.lock().await.id else {
            return;
        };
        if let Ok(stored) = self.store.load_session(sid).await
            && let Some(leaf_id) = stored.leaf_id
            && let Some(leaf) = stored.messages.iter().find(|m| m.id == leaf_id)
            && leaf.role == shuvarie_db::MsgRole::Assistant
        {
            let _ = self.store.delete_tool_calls_for_message(leaf.id).await;
            let _ = self.store.delete_message(leaf.id).await;
            let _ = self.store.set_active_leaf(sid, leaf.parent_id).await;
        }
        let loaded = match self.store.load_session(sid).await {
            Ok(stored) => Session::from_stored(stored),
            Err(_) => return,
        };
        let content = loaded.messages.last().map(|m| m.content.clone());
        *s.lock().await = loaded.clone();
        let _ = self
            .event_tx
            .send(Event::Forked {
                session: loaded,
                prompt: None,
            })
            .await;
        if let Some(content) = content {
            self.self_replay_send(content, false, None).await;
        }
    }
}

/// Resolve a custom command's `model` spec (`<provider_type>/<model>`) to the
/// provider connection id + model id to stream on. The provider entry comes
/// from the `default-providers` config: the last `use` entry whose connection
/// still exists and whose `kind` resolves to the requested type wins; with no
/// matching entry, the first configured provider of that type (lowest id) is
/// used. A failed resolution reports the reason as an error.
fn resolve_model_override(
    connections: &Connections,
    default_providers: &shuvarie_config::DefaultProvidersConfig,
    spec: &str,
) -> Result<(String, String), String> {
    let Some((type_name, model)) = crate::custom_commands::parse_model_spec(spec) else {
        return Err(format!(
            "invalid model `{spec}` (expected `<provider>/<model>`)"
        ));
    };
    let Some(target) = crate::catalog::parse_provider_type(type_name) else {
        return Err(format!(
            "unknown provider type `{type_name}` in model `{spec}`"
        ));
    };
    for id in default_providers.use_ids.iter().rev() {
        if connections
            .providers
            .get(id)
            .is_some_and(|provider| crate::catalog::provider_type(&provider.kind) == target)
        {
            return Ok((id.clone(), model.to_string()));
        }
    }
    for (id, provider) in &connections.providers {
        if crate::catalog::provider_type(&provider.kind) == target {
            return Ok((id.clone(), model.to_string()));
        }
    }
    Err(format!(
        "no provider connection of type `{type_name}` is configured (for model `{model}`)"
    ))
}

async fn load_startup_session(
    store: &mut Store,
    session: &mut Option<Arc<Mutex<Session>>>,
    locked: &mut Option<uuid::Uuid>,
    event_tx: &Sender<Event>,
    startup: StartupSession,
) {
    let stored = match startup {
        StartupSession::None => return,
        StartupSession::MostRecent => match store.most_recent_session().await {
            Ok(stored) => stored,
            Err(e) => {
                let _ = event_tx
                    .send(Event::SessionError {
                        error: e.to_string(),
                    })
                    .await;
                return;
            }
        },
        StartupSession::Session(id) => match store.load_session(id).await {
            Ok(stored) => Some(stored),
            Err(e) => {
                let _ = event_tx
                    .send(Event::SessionError {
                        error: e.to_string(),
                    })
                    .await;
                return;
            }
        },
    };
    let Some(stored) = stored else { return };
    let id = stored.id;
    match store.acquire_session_lock(id, now_ms()).await {
        Ok(LockAcquire::Held) => {
            let _ = event_tx.send(Event::SessionLocked { id }).await;
            return;
        }
        Ok(_) => *locked = Some(id),
        Err(_) => {}
    }
    let loaded = Session::from_stored(stored);
    *session = Some(Arc::new(Mutex::new(loaded.clone())));
    let _ = event_tx
        .send(Event::SessionLoaded {
            id,
            title: loaded.title.clone().unwrap_or_default(),
            session: loaded,
        })
        .await;
}

async fn stream_busy(active_stream: &Option<AbortHandle>, event_tx: &Sender<Event>) -> bool {
    if active_stream.as_ref().is_some_and(|h| !h.is_finished()) {
        let _ = event_tx
            .send(Event::StreamError {
                error: "a reply is already streaming".into(),
            })
            .await;
        return true;
    }
    false
}

/// Cut the running stream and settle its in-flight state: persist the
/// partial turn as interrupted, dismiss pending asks, and surface the
/// cancellation. Returns `true` when a running stream was cut (the caller
/// then resets the steer signal); `false` while the stream is idle or already
/// self-cutting a steered dispatch (`FINALIZING`) — the ending task owns the
/// persist and still reports its outcome.
async fn cut_running_stream(
    ctx: &mut CoreCtx,
    pending_questions: &mut HashMap<u64, oneshot::Sender<AnswerResponse>>,
    pending_permissions: &mut HashMap<u64, oneshot::Sender<PermissionAnswer>>,
) -> bool {
    let Some(handle) = ctx.active_stream.take() else {
        return false;
    };
    if handle.is_finished() {
        return false;
    }
    if ctx.steer.is_finalizing() {
        // The stream task is already cutting itself at an action boundary to
        // dispatch a steered prompt; it persists the turn and reports
        // `StreamOutcome::Preempted`. Keep the handle so the ending task still
        // counts as busy and don't abort it mid-persist.
        ctx.active_stream = Some(handle);
        return false;
    }
    handle.abort();
    dismiss_pending_questions(pending_questions);
    dismiss_pending_permissions(pending_permissions);
    persist_interrupted_turn(
        ctx.turn_state.take(),
        &mut ctx.store,
        &ctx.session,
        &ctx.event_tx,
    )
    .await;
    let _ = ctx.event_tx.send(Event::StreamCancelled).await;
    true
}

/// Wipe the steered queue (session-level transition) and tell the TUI to drop
/// its queued-prompt display.
async fn clear_steered(
    steered: &mut Vec<(String, Option<String>)>,
    steer: &SteerSignal,
    event_tx: &Sender<Event>,
) {
    steer.disarm();
    if steered.is_empty() {
        return;
    }
    steered.clear();
    let _ = event_tx.send(Event::SteeredCleared).await;
}

/// Settle all pending questions as dismissed (dropping the responder makes
/// the awaiting tool error out with "The user dismissed this question").
fn dismiss_pending_questions(
    pending_questions: &mut HashMap<u64, oneshot::Sender<AnswerResponse>>,
) {
    pending_questions.clear();
}

/// Settle all pending permission asks as denied (dropping the responder makes
/// the awaiting tool error out with a user-denied message).
fn dismiss_pending_permissions(
    pending_permissions: &mut HashMap<u64, oneshot::Sender<PermissionAnswer>>,
) {
    pending_permissions.clear();
}

fn client_for<'a>(
    clients: &'a mut HashMap<String, ProviderClient>,
    connections: &'a mut Connections,
    event_tx: &Sender<Event>,
    name: &str,
) -> Result<&'a ProviderClient, String> {
    if !clients.contains_key(name) {
        let pc = connections
            .providers
            .get(name)
            .ok_or_else(|| format!("provider '{name}' not found"))?;
        let client = build_client(pc, event_tx)?;
        clients.insert(name.to_string(), client);
    }
    Ok(clients.get(name).unwrap())
}

/// Whether `item` opens the next turn action while `action` has just
/// completed one — the boundary at which a queued steered prompt cuts in.
/// A boundary is a completed action followed by a different action starting:
/// mid-segment deltas (thinking streaming into more thinking, text into more
/// text) continue the running action and never cut, so a steered prompt is
/// sent after each completed action rather than mid-action. The first action
/// of a turn (Fresh) cannot be preempted and a running tool batch (Tools)
/// must settle first; bookkeeping items (usage, worker activity, empty
/// deltas) never start an action.
fn starts_action_after_boundary(item: &shuvarie_llm::StreamItem, action: &ActionPhase) -> bool {
    if matches!(action, ActionPhase::Fresh | ActionPhase::Tools) {
        return false;
    }
    let starting = match item {
        shuvarie_llm::StreamItem::Delta { text } if !text.is_empty() => Some(ActionPhase::Text),
        shuvarie_llm::StreamItem::Reasoning { text } if !text.is_empty() => {
            Some(ActionPhase::Thinking)
        }
        shuvarie_llm::StreamItem::ToolStart { worker: None, .. } => Some(ActionPhase::Tools),
        shuvarie_llm::StreamItem::WorkerStart { .. } => Some(ActionPhase::Tools),
        _ => None,
    };
    match (action, starting) {
        // Between actions any next main-stream action starting is the
        // boundary (the settle check below dispatches on completion alone).
        (ActionPhase::Between, Some(_)) => true,
        // Mid-segment deltas continue the running action; only an item
        // starting a different action means the running one completed.
        (ActionPhase::Thinking, Some(started)) => started != ActionPhase::Thinking,
        (ActionPhase::Text, Some(started)) => started != ActionPhase::Text,
        _ => false,
    }
}

/// Resolve a streamed shell chunk to the still-running `run_shell` call it
/// belongs to. Candidates are the pending calls of the chunk's agent (the main
/// map for `worker: None`, the worker-internal map otherwise) whose name and
/// serialized command match the chunk; a single match identifies the call, an
/// ambiguous or already-settled call stays unresolved.
fn resolve_shell_call(
    worker: &Option<String>,
    command: &str,
    pending_tool_args: &std::collections::HashMap<String, PendingTool>,
    pending_worker_tools: &std::collections::HashMap<String, PendingTool>,
) -> Option<String> {
    let matched: Vec<&String> = match worker {
        None => pending_tool_args
            .iter()
            .filter(|(_, pending)| {
                pending.name == "run_shell"
                    && args_command(&pending.args_json).as_deref() == Some(command)
            })
            .map(|(id, _)| id)
            .collect(),
        Some(_) => pending_worker_tools
            .iter()
            .filter(|(_, pending)| {
                &pending.worker == worker
                    && pending.name == "run_shell"
                    && args_command(&pending.args_json).as_deref() == Some(command)
            })
            .map(|(id, _)| id)
            .collect(),
    };
    match matched.as_slice() {
        [id] => Some((*id).clone()),
        _ => None,
    }
}

fn args_command(args_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(args_json)
        .ok()?
        .get("command")?
        .as_str()
        .map(str::to_owned)
}

/// Merge the turn's live `run_shell` output channel into the turn stream:
/// chunks become `StreamItem::ShellOutput` items the stream loop handles like
/// any other item. `select_all` polls the LLM/worker sub-streams first, so a
/// call's `ToolStart` is always processed before its own chunks and the chunk
/// resolves to its call id.
fn merge_shell_chunks(
    stream: shuvarie_llm::StreamStream,
    shell_rx: tokio::sync::mpsc::Receiver<crate::tools::ShellChunk>,
) -> shuvarie_llm::StreamStream {
    let chunks = futures_util::stream::unfold(shell_rx, |mut rx| async move {
        let chunk = rx.recv().await?;
        Some((
            shuvarie_llm::StreamItem::ShellOutput {
                worker: chunk.worker,
                command: chunk.command,
                stdout: chunk.stdout,
                stderr: chunk.stderr,
            },
            rx,
        ))
    });
    Box::pin(futures_util::stream::select_all([stream, Box::pin(chunks)]))
}

#[allow(clippy::too_many_arguments)]
async fn stream_stream_to_events(
    mut stream: shuvarie_llm::StreamStream,
    session: Arc<Mutex<Session>>,
    client: ProviderClient,
    catalog_provider: Option<selune::Provider>,
    mut store: Store,
    model: String,
    keep_recent_tokens: u64,
    worker_usage: Arc<std::sync::Mutex<shuvarie_llm::TokenUsage>>,
    embedding_setup: Option<EmbeddingSetup>,
    event_tx: Sender<Event>,
    turn_state: Arc<Mutex<TurnState>>,
    stream_done_tx: Sender<StreamOutcome>,
    steer: SteerSignal,
    deny_cut: crate::permissions::DenyCut,
) {
    use futures_util::StreamExt;

    let mut assistant_message_id: Option<u64> = None;
    let mut assistant_seq: u64 = 0;
    let mut pending_reasoning: Vec<shuvarie_db::ReasoningSegment> = Vec::new();
    let mut reasoning_started: Option<std::time::Instant> = None;
    let mut text_segments: Vec<shuvarie_db::TextSegment> = Vec::new();
    let mut tool_seq: u64 = 0;
    let mut turn_tool_records: Vec<crate::tool_record::ToolRecord> = Vec::new();
    let mut pending_tool_args: std::collections::HashMap<String, PendingTool> =
        std::collections::HashMap::new();
    // Worker-internal tool calls tracked apart from the main agent's batch:
    // a worker's rig run can wedge its own pair (erroring mid-tool) or a
    // result can straggle past the worker's `WorkerResult` on the merged
    // stream, and neither may keep the main batch from settling.
    let mut pending_worker_tools: std::collections::HashMap<String, PendingTool> =
        std::collections::HashMap::new();
    let mut pending_worker_starts: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    let mut outcome = StreamOutcome::Finished;
    // Set when the manager stream reaches `Done`. Sending `Event::StreamDone`
    // is deferred until the merged stream is exhausted, so worker receiver
    // items queued behind it (a slow subagent's final tool results) are
    // delivered to the TUI before the turn is committed.
    let mut done: Option<(String, TokenUsage)> = None;
    // Which turn action the main stream is in. Worker-internal activity and
    // bookkeeping items (usage) never move it, so steering can only cut in
    // between the agent's own actions.
    let mut action = ActionPhase::Fresh;

    while let Some(item) = stream.next().await {
        if starts_action_after_boundary(&item, &action) && steer.is_armed() && steer.begin_preempt()
        {
            done = None;
            outcome = cut_turn_cancelled(&turn_state, &mut store, &session, &event_tx).await;
            break;
        }
        match item {
            shuvarie_llm::StreamItem::Delta { text } if !text.is_empty() => {
                let after_tool = turn_tool_records.len() as u64;
                match text_segments.last_mut() {
                    Some(segment) if segment.after_tool == after_tool => {
                        segment.text.push_str(&text);
                    }
                    _ => {
                        text_segments.push(shuvarie_db::TextSegment {
                            after_tool,
                            text: text.clone(),
                        });
                    }
                }
                {
                    let mut ts = turn_state.lock().await;
                    ts.text_segments = text_segments.clone();
                }
                let _ = event_tx.send(Event::TokenReceived { content: text }).await;
                action = ActionPhase::Text;
            }
            shuvarie_llm::StreamItem::Delta { .. } => {}
            shuvarie_llm::StreamItem::Reasoning { text } if !text.is_empty() => {
                let after_tool = turn_tool_records.len() as u64;
                let now = std::time::Instant::now();
                match pending_reasoning.last_mut() {
                    Some(segment) if segment.after_tool == after_tool => {
                        segment.text.push_str(&text);
                    }
                    _ => {
                        pending_reasoning.push(shuvarie_db::ReasoningSegment {
                            after_tool,
                            text: text.clone(),
                            duration_ms: 0,
                        });
                        reasoning_started = Some(now);
                    }
                }
                if let (Some(segment), Some(started)) =
                    (pending_reasoning.last_mut(), reasoning_started)
                {
                    segment.duration_ms = started.elapsed().as_millis() as u64;
                }
                {
                    let mut ts = turn_state.lock().await;
                    ts.pending_reasoning = pending_reasoning.clone();
                }
                let _ = event_tx
                    .send(Event::ReasoningReceived { content: text })
                    .await;
                action = ActionPhase::Thinking;
            }
            shuvarie_llm::StreamItem::Reasoning { .. } => {}
            shuvarie_llm::StreamItem::ToolStart {
                name,
                args,
                worker,
                call_id,
            } => {
                let is_main = worker.is_none();
                let started = std::time::Instant::now();
                if is_main {
                    pending_tool_args.insert(
                        call_id.clone(),
                        PendingTool {
                            name: name.clone(),
                            worker: None,
                            args_json: args.to_string(),
                            started,
                        },
                    );
                } else {
                    pending_worker_tools.insert(
                        call_id.clone(),
                        PendingTool {
                            name: name.clone(),
                            worker: worker.clone(),
                            args_json: args.to_string(),
                            started,
                        },
                    );
                }
                ensure_assistant_row(
                    &mut assistant_message_id,
                    &mut assistant_seq,
                    &session,
                    &mut store,
                    &pending_reasoning,
                    &event_tx,
                )
                .await;
                {
                    let mut ts = turn_state.lock().await;
                    ts.assistant_message_id = assistant_message_id;
                    ts.assistant_seq = assistant_seq;
                    ts.pending_tools.push(PendingToolCall {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        args_json: args.to_string(),
                        worker: worker.clone(),
                        started,
                    });
                }
                let _ = event_tx
                    .send(Event::ToolStarted {
                        name,
                        args,
                        worker,
                        call_id,
                    })
                    .await;
                if is_main {
                    action = ActionPhase::Tools;
                }
            }
            shuvarie_llm::StreamItem::ToolResult {
                name,
                output,
                ok,
                worker,
                file_change,
                streams,
                call_id,
            } => {
                let (fc_json, original, new) = serialize_file_change(&file_change);
                let (display_output, display_stderr) = match &streams {
                    Some(s) => (s.stdout.clone(), s.stderr.clone()),
                    None => (output.clone(), String::new()),
                };
                let (args_json, duration_ms) = if worker.is_none() {
                    match pending_tool_args.remove(&call_id) {
                        Some(pending) => (
                            pending.args_json,
                            pending.started.elapsed().as_millis() as u64,
                        ),
                        None => (String::new(), 0),
                    }
                } else {
                    match pending_worker_tools.remove(&call_id) {
                        Some(pending) => (
                            pending.args_json,
                            pending.started.elapsed().as_millis() as u64,
                        ),
                        None => (String::new(), 0),
                    }
                };
                let worker_name = worker.as_deref();
                let is_main = worker.is_none();
                if let Some(msg_id) = assistant_message_id {
                    let session_id = session.lock().await.id;
                    if let Some(sid) = session_id {
                        let _ = store
                            .append_tool_call(
                                sid,
                                msg_id,
                                tool_seq,
                                &name,
                                &args_json,
                                &display_output,
                                &display_stderr,
                                ok,
                                false,
                                worker_name,
                                &fc_json,
                                original.as_deref(),
                                new.as_deref(),
                                duration_ms,
                            )
                            .await;
                    }
                    tool_seq += 1;
                }
                turn_tool_records.push(crate::tool_record::ToolRecord {
                    name: name.clone(),
                    args_json,
                    output: display_output,
                    stderr: display_stderr,
                    ok,
                    killed: false,
                    worker: worker.clone(),
                    message_id: assistant_message_id.unwrap_or_default(),
                    message_seq: assistant_seq,
                    file_change: file_change.clone(),
                    original_content: original.clone(),
                    new_content: new.clone(),
                    duration_ms,
                });
                {
                    let mut ts = turn_state.lock().await;
                    ts.pending_tools.retain(|pending| {
                        !(pending.call_id == call_id && pending.worker == worker)
                    });
                    ts.tool_records = turn_tool_records.clone();
                }
                let _ = event_tx
                    .send(Event::ToolFinished {
                        name,
                        ok,
                        output,
                        worker,
                        file_change,
                        streams,
                        duration_ms,
                        call_id,
                    })
                    .await;
                if is_main {
                    action = if pending_tool_args.is_empty() && pending_worker_starts.is_empty() {
                        ActionPhase::Between
                    } else {
                        ActionPhase::Tools
                    };
                }
            }
            shuvarie_llm::StreamItem::WorkerStart {
                name,
                args,
                call_id,
            } => {
                pending_worker_starts.insert(call_id.clone(), std::time::Instant::now());
                let _ = event_tx
                    .send(Event::WorkerStarted {
                        name,
                        args,
                        call_id,
                    })
                    .await;
                action = ActionPhase::Tools;
            }
            shuvarie_llm::StreamItem::WorkerResult {
                name,
                output,
                ok,
                call_id,
            } => {
                let duration_ms = pending_worker_starts
                    .remove(&call_id)
                    .map(|started| started.elapsed().as_millis() as u64)
                    .unwrap_or_default();
                let _ = event_tx
                    .send(Event::WorkerFinished {
                        name,
                        ok,
                        output,
                        duration_ms,
                        call_id,
                    })
                    .await;
                action = if pending_tool_args.is_empty() && pending_worker_starts.is_empty() {
                    ActionPhase::Between
                } else {
                    ActionPhase::Tools
                };
            }
            shuvarie_llm::StreamItem::ShellOutput {
                worker,
                command,
                stdout,
                stderr,
            } => {
                // The chunk belongs to one specific `run_shell` call: resolve
                // it against the still-running calls' args (keyed by call id)
                // so concurrent shells of one agent stream into their own
                // blocks. Ambiguous (two identical commands) or already-settled
                // calls resolve to `None`; the TUI then falls back to the
                // name+worker match.
                let call_id = resolve_shell_call(
                    &worker,
                    &command,
                    &pending_tool_args,
                    &pending_worker_tools,
                );
                let _ = event_tx
                    .send(Event::ToolOutput {
                        tool: "run_shell".to_string(),
                        worker,
                        call_id,
                        stdout,
                        stderr,
                    })
                    .await;
            }
            shuvarie_llm::StreamItem::Usage { usage, worker } => {
                let cost = catalog_provider
                    .as_ref()
                    .map(|p| crate::catalog::estimate_cost(p, &model, &usage))
                    .unwrap_or(0.0);
                let context_tokens = if worker.is_none() {
                    let footprint = shuvarie_llm::context_footprint(&usage);
                    (footprint > 0).then_some(footprint)
                } else {
                    None
                };
                let _ = event_tx
                    .send(Event::UsageUpdate {
                        usage,
                        cost,
                        context_tokens,
                    })
                    .await;
            }
            shuvarie_llm::StreamItem::Done { text, usage } => {
                let text = if text.is_empty() && !text_segments.is_empty() {
                    shuvarie_db::join_text_segments(&text_segments)
                } else {
                    text
                };
                done = Some((text, usage));
                // Keep consuming: `select_all` continues past the ended main
                // stream and drains the worker receivers before returning
                // `None`.
            }
            shuvarie_llm::StreamItem::ConnectionError { message, reason } => {
                persist_stream_error(
                    assistant_message_id,
                    &text_segments,
                    &pending_reasoning,
                    &session,
                    &mut store,
                )
                .await;
                done = None;
                outcome = StreamOutcome::ConnectionLost { reason, message };
                break;
            }
            shuvarie_llm::StreamItem::Error { message } => {
                persist_stream_error(
                    assistant_message_id,
                    &text_segments,
                    &pending_reasoning,
                    &session,
                    &mut store,
                )
                .await;
                let _ = event_tx.send(Event::StreamError { error: message }).await;
                done = None;
                break;
            }
            shuvarie_llm::StreamItem::Overflow => {
                persist_stream_error(
                    assistant_message_id,
                    &text_segments,
                    &pending_reasoning,
                    &session,
                    &mut store,
                )
                .await;
                let _ = event_tx
                    .send(Event::StreamError {
                        error:
                            "context budget exceeded — compacting session history and continuing"
                                .into(),
                    })
                    .await;
                // Run compaction: summarize the head of the active path so
                // the next turn sends [summary, tail] instead of the full
                // history. The summary is inserted into the chain at the cut
                // point and the first tail message reparented under it.
                let mut compacted = false;
                if let Some(sid) = session.lock().await.id
                    && let Ok(stored) = store.load_session(sid).await
                {
                    let chain = chain_of(&stored);
                    let by_id: HashMap<u64, &shuvarie_db::StoredMessage> =
                        stored.messages.iter().map(|m| (m.id, m)).collect();
                    let path: Vec<shuvarie_db::StoredMessage> = chain
                        .iter()
                        .filter_map(|id| by_id.get(id).map(|m| (*m).clone()))
                        .collect();
                    if let Some(plan) = crate::compaction::select_plan(&path, keep_recent_tokens) {
                        let head = &path[plan.start..plan.cut];
                        let chain_index: std::collections::HashMap<u64, usize> =
                            chain.iter().enumerate().map(|(i, id)| (*id, i)).collect();
                        let records =
                            span_tool_records(&stored, &chain_index, plan.start..plan.cut);
                        let head_text =
                            crate::compaction::serialize_head(head, &records, plan.start);
                        let _ = event_tx.send(Event::CompactionStarted).await;
                        let summary =
                            crate::compaction::summarize(&client, &model, &head_text).await;
                        let _ = event_tx.send(Event::CompactionFinished).await;
                        match summary {
                            Ok(summary) => {
                                let parent = path[plan.cut - 1].id;
                                if let Ok(msg) =
                                    store.append_summary(sid, Some(parent), &summary).await
                                {
                                    if let Some(first_tail) = path.get(plan.cut) {
                                        let _ = store
                                            .set_message_parent(first_tail.id, Some(msg.id))
                                            .await;
                                    }
                                    compacted = true;
                                }
                            }
                            Err(e) => {
                                let _ = event_tx
                                    .send(Event::StreamError {
                                        error: format!("compaction failed: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                }
                outcome = StreamOutcome::Overflowed { compacted };
                done = None;
                break;
            }
        }
        // A settled tool batch is itself a completed action: dispatch the
        // queued prompt right here instead of waiting for the next request's
        // first item (provider latency can silence the stream for seconds).
        // Skipped once the final `Done` was seen so a finished turn still
        // commits cleanly, and while a worker-internal result is still
        // straggling so it surfaces before the cut (a leaked one never
        // settles and must not wedge the phase).
        if done.is_none()
            && matches!(action, ActionPhase::Between)
            && pending_worker_tools.is_empty()
            && steer.is_armed()
            && steer.begin_preempt()
        {
            done = None;
            outcome = cut_turn_cancelled(&turn_state, &mut store, &session, &event_tx).await;
            break;
        }
        // A permission denial (rule deny or user rejection) ends the turn
        // like a user cancel: the denied call's result was just persisted, so
        // the denial reason stays visible in its block. Skipped once the
        // final `Done` was seen so a straggling worker denial cannot discard
        // a finished turn's reply.
        if done.is_none() && deny_cut.is_set() {
            deny_cut.take();
            done = None;
            outcome = cut_turn_cancelled(&turn_state, &mut store, &session, &event_tx).await;
            break;
        }
    }

    if let Some((text, usage)) = done {
        let mut guard = session.lock().await;
        let combined = {
            let worker_usage = worker_usage.lock().unwrap();
            usage + *worker_usage
        };
        guard.push_assistant(text.clone());
        let cost = catalog_provider
            .as_ref()
            .map(|p| crate::catalog::estimate_cost(p, &model, &combined))
            .unwrap_or(0.0);
        guard.add_usage(combined, cost);
        let usage_snapshot = guard.usage();
        let cost_snapshot = guard.cost;
        let id = guard.id;
        let seq = guard.messages.len() - 1;
        let reasoning = pending_reasoning.clone();
        guard.reasoning.insert(seq as u64, reasoning.clone());
        if !text_segments.is_empty() {
            guard
                .text_segments
                .insert(seq as u64, text_segments.clone());
        }
        guard.tool_records.append(&mut turn_tool_records);
        let parent = guard.leaf_id;
        drop(guard);
        if let Some(id) = id {
            if let Some(msg_id) = assistant_message_id {
                let _ = store
                    .update_message(
                        msg_id,
                        &text,
                        &reasoning,
                        &text_segments,
                        false,
                        combined,
                        cost,
                        &usage,
                    )
                    .await;
                if let Some(setup) = &embedding_setup {
                    let store_idx = store.clone();
                    let setup_idx = setup.clone();
                    let content_idx = text.clone();
                    tokio::spawn(async move {
                        let _ = embeddings::index_message(
                            &mut store_idx.clone(),
                            &setup_idx,
                            msg_id,
                            id,
                            seq as u64,
                            &content_idx,
                        )
                        .await;
                    });
                }
            } else {
                match store
                    .append_assistant_message(
                        id,
                        parent,
                        &text,
                        &reasoning,
                        &text_segments,
                        false,
                        combined,
                        cost,
                        &usage,
                    )
                    .await
                {
                    Ok(msg) => {
                        session.lock().await.leaf_id = Some(msg.id);
                        if let Some(setup) = &embedding_setup {
                            let store_idx = store.clone();
                            let setup_idx = setup.clone();
                            let content_idx = msg.content.clone();
                            tokio::spawn(async move {
                                let _ = embeddings::index_message(
                                    &mut store_idx.clone(),
                                    &setup_idx,
                                    msg.id,
                                    id,
                                    seq as u64,
                                    &content_idx,
                                )
                                .await;
                            });
                        }
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::StreamError {
                                error: format!("failed to persist message: {e}"),
                            })
                            .await;
                    }
                }
            }
        }
        let _ = event_tx.send(Event::StreamDone { text, usage }).await;
        let _ = event_tx
            .send(Event::UsageSnapshot {
                usage: usage_snapshot,
                cost: cost_snapshot,
            })
            .await;
    }
    let _ = stream_done_tx.send(outcome).await;
}

/// Tool records whose assistant message falls in the compaction span, with
/// message indices matching the walked path slice so the transcript
/// serializer can attach them to their messages.
fn span_tool_records(
    stored: &shuvarie_db::StoredSession,
    chain_index: &std::collections::HashMap<u64, usize>,
    span: std::ops::Range<usize>,
) -> Vec<crate::tool_record::ToolRecord> {
    stored
        .tool_calls
        .iter()
        .filter(|tc| {
            chain_index
                .get(&tc.message_id)
                .is_some_and(|&idx| span.contains(&idx))
        })
        .map(|tc| {
            let mut record = crate::tool_record::ToolRecord::from_stored(tc.clone());
            if let Some(&idx) = chain_index.get(&record.message_id) {
                record.message_seq = idx as u64;
            }
            record
        })
        .collect()
}

async fn ensure_assistant_row(
    assistant_message_id: &mut Option<u64>,
    assistant_seq: &mut u64,
    session: &Arc<Mutex<Session>>,
    store: &mut Store,
    reasoning: &[shuvarie_db::ReasoningSegment],
    _event_tx: &Sender<Event>,
) {
    if assistant_message_id.is_some() {
        return;
    }
    let (id, parent) = {
        let guard = session.lock().await;
        (guard.id, guard.leaf_id)
    };
    let Some(id) = id else {
        return;
    };
    if let Ok(msg) = store
        .append_assistant_message(
            id,
            parent,
            "",
            reasoning,
            &[],
            false,
            shuvarie_llm::TokenUsage::default(),
            0.0,
            &shuvarie_llm::TokenUsage::default(),
        )
        .await
    {
        *assistant_message_id = Some(msg.id);
        *assistant_seq = msg.seq;
        session.lock().await.leaf_id = Some(msg.id);
    }
}

fn serialize_file_change(fc: &Option<FileChange>) -> (String, Option<String>, Option<String>) {
    match fc {
        Some(change) => {
            let json = serde_json::to_string(change).unwrap_or_default();
            (json, change.original_content(), change.new_content())
        }
        None => (String::new(), None, None),
    }
}

/// Killed records for the tool calls of an interrupted turn that never
/// returned: no output, `ok: false`, flagged killed, duration up to the cut.
fn killed_records(
    pending: &[PendingToolCall],
    message_id: u64,
    message_seq: u64,
) -> Vec<crate::tool_record::ToolRecord> {
    pending
        .iter()
        .map(|pending| crate::tool_record::ToolRecord {
            name: pending.name.clone(),
            args_json: pending.args_json.clone(),
            output: String::new(),
            stderr: String::new(),
            ok: false,
            killed: true,
            worker: pending.worker.clone(),
            message_id,
            message_seq,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: pending.started.elapsed().as_millis() as u64,
        })
        .collect()
}

/// Cut the stream for a queued steered prompt or a permission denial:
/// persist the partial turn as interrupted, surface the cancellation, and
/// report `Preempted` back to the run loop so it dispatches any queued
/// prompt.
async fn cut_turn_cancelled(
    turn_state: &Arc<Mutex<TurnState>>,
    store: &mut Store,
    session: &Arc<Mutex<Session>>,
    event_tx: &Sender<Event>,
) -> StreamOutcome {
    persist_interrupted_turn(
        Some(turn_state.clone()),
        store,
        &Some(session.clone()),
        event_tx,
    )
    .await;
    let _ = event_tx.send(Event::StreamCancelled).await;
    StreamOutcome::Preempted
}

async fn persist_interrupted_turn(
    turn_state: Option<Arc<Mutex<TurnState>>>,
    store: &mut Store,
    session: &Option<Arc<Mutex<Session>>>,
    _event_tx: &Sender<Event>,
) {
    let (text_segments, reasoning, msg_id, tool_records, pending_tools, assistant_seq) =
        match turn_state {
            Some(ts_arc) => {
                let ts = ts_arc.lock().await;
                (
                    ts.text_segments.clone(),
                    ts.pending_reasoning.clone(),
                    ts.assistant_message_id,
                    ts.tool_records.clone(),
                    ts.pending_tools.clone(),
                    ts.assistant_seq,
                )
            }
            None => return,
        };
    let text = shuvarie_db::join_text_segments(&text_segments);

    let Some(s) = session else {
        return;
    };
    let guard = s.lock().await;
    let Some(id) = guard.id else {
        return;
    };
    drop(guard);

    if let Some(msg_id) = msg_id {
        let _ = store
            .update_message(
                msg_id,
                &text,
                &reasoning,
                &text_segments,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
                &shuvarie_llm::TokenUsage::default(),
            )
            .await;
        let killed = killed_records(&pending_tools, msg_id, assistant_seq);
        for (i, record) in killed.iter().enumerate() {
            let _ = store
                .append_tool_call(
                    id,
                    msg_id,
                    tool_records.len() as u64 + i as u64,
                    &record.name,
                    &record.args_json,
                    &record.output,
                    &record.stderr,
                    record.ok,
                    record.killed,
                    record.worker.as_deref(),
                    "",
                    None,
                    None,
                    record.duration_ms,
                )
                .await;
        }
        {
            let mut g = s.lock().await;
            g.push_assistant(text.clone());
            let seq = g.messages.len() - 1;
            g.reasoning.insert(seq as u64, reasoning);
            if !text_segments.is_empty() {
                g.text_segments.insert(seq as u64, text_segments);
            }
            g.interrupted.insert(seq as u64, true);
            g.tool_records.extend(tool_records);
            g.tool_records.extend(killed);
        }
    } else if !text.is_empty() || !reasoning.is_empty() {
        let parent = { s.lock().await.leaf_id };
        if let Ok(msg) = store
            .append_assistant_message(
                id,
                parent,
                &text,
                &reasoning,
                &text_segments,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
                &shuvarie_llm::TokenUsage::default(),
            )
            .await
        {
            let mut g = s.lock().await;
            g.leaf_id = Some(msg.id);
            g.push_assistant(text);
            let seq = g.messages.len() - 1;
            if !reasoning.is_empty() {
                g.reasoning.insert(seq as u64, reasoning.clone());
            }
            if !text_segments.is_empty() {
                g.text_segments.insert(seq as u64, text_segments);
            }
            g.interrupted.insert(seq as u64, true);
        }
    }
}

/// Write the active session to a JSON file: an explicit path, a directory
/// (the default file name is joined under it), or the default file name in
/// the working directory.
async fn export_session(
    store: &mut Store,
    session_id: uuid::Uuid,
    path: Option<&std::path::Path>,
) -> shuvarie_db::Result<std::path::PathBuf> {
    let stored = store.load_session(session_id).await?;
    let file = shuvarie_db::SessionFile::from_stored(&stored);
    let path = shuvarie_db::session_file::resolve_export_path(path, session_id);
    file.write_json(&path)?;
    Ok(path)
}

fn build_client(pc: &ProviderConfig, event_tx: &Sender<Event>) -> Result<ProviderClient, String> {
    let kind = crate::catalog::provider_type(&pc.kind);
    let base_url = crate::catalog::base_url_for(&pc.kind, pc.base_url.as_deref());
    let on_device_code = device_code_handler(kind, pc.name.clone(), event_tx);
    ProviderClient::build_with_device_code(
        kind,
        pc.api_key.as_deref(),
        base_url.as_deref(),
        on_device_code,
    )
    .map_err(|e| e.to_string())
}

/// The device-code prompt handler for the transports rig runs the OAuth2
/// device flow on (ChatGPT, Copilot): forwards each sign-in prompt to the TUI
/// as [`Event::AuthPrompt`]. `try_send` because the callback runs deep inside
/// the streaming stack; a full event channel drops the prompt and the flow
/// simply times out later. `None` for every other transport.
pub(crate) fn device_code_handler(
    kind: selune::ProviderType,
    provider: String,
    event_tx: &Sender<Event>,
) -> Option<DeviceCodeHandler> {
    if !crate::catalog::supports_device_flow(kind) {
        return None;
    }
    let event_tx = event_tx.clone();
    Some(Arc::new(move |prompt: shuvarie_llm::DeviceCodePrompt| {
        let _ = event_tx.try_send(Event::AuthPrompt {
            provider: provider.clone(),
            verification_uri: prompt.verification_uri,
            user_code: prompt.user_code,
        });
    }))
}

/// Handle [`Command::AuthProviderLogin`]: drive sign-in for the provider to
/// completion — a cached or pasted credential resolves immediately, a missing
/// one runs the interactive device flow. An already-cached client is reused so
/// concurrent auth (e.g. a model listing that kicked the flow off) shares one
/// serialized device flow; otherwise a temporary client is built and the
/// token lands in the shared on-disk cache for later clients. The flow polls
/// for minutes while the user authorizes in a browser, so it runs off the
/// command loop and reports its outcome as [`Event::AuthSuccess`] /
/// [`Event::AuthFailed`], tagged with the provider's display name (the same
/// identifier [`Event::AuthPrompt`] carries). Unknown providers and
/// client-build failures report immediately.
async fn handle_auth_provider_login(
    name: String,
    providers: &BTreeMap<String, ProviderConfig>,
    clients: &HashMap<String, ProviderClient>,
    event_tx: &Sender<Event>,
) {
    let Some(pc) = providers.get(&name) else {
        let error = format!("unknown provider '{name}'");
        let _ = event_tx
            .send(Event::AuthFailed {
                provider: name,
                error,
            })
            .await;
        return;
    };
    let display = pc.name.clone();
    let client = match clients.get(&name) {
        Some(client) => client.clone(),
        None => match build_client(pc, event_tx) {
            Ok(client) => client,
            Err(error) => {
                let _ = event_tx
                    .send(Event::AuthFailed {
                        provider: display,
                        error,
                    })
                    .await;
                return;
            }
        },
    };
    let event_tx = event_tx.clone();
    tokio::spawn(async move {
        let outcome = match client.authorize().await {
            Ok(()) => Event::AuthSuccess {
                provider: display.clone(),
            },
            Err(e) => Event::AuthFailed {
                provider: display.clone(),
                error: e.to_string(),
            },
        };
        let _ = event_tx.send(outcome).await;
    });
}

async fn persist_stream_error(
    assistant_message_id: Option<u64>,
    text_segments: &[shuvarie_db::TextSegment],
    pending_reasoning: &[shuvarie_db::ReasoningSegment],
    session: &Arc<Mutex<Session>>,
    store: &mut Store,
) {
    let text = shuvarie_db::join_text_segments(text_segments);
    if text.is_empty() && pending_reasoning.is_empty() {
        return;
    }
    let guard = session.lock().await;
    let id = guard.id;
    drop(guard);
    let Some(id) = id else {
        return;
    };
    if let Some(msg_id) = assistant_message_id {
        let _ = store
            .update_message(
                msg_id,
                &text,
                pending_reasoning,
                text_segments,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
                &shuvarie_llm::TokenUsage::default(),
            )
            .await;
    } else if !text.is_empty() {
        let parent = { session.lock().await.leaf_id };
        if let Ok(msg) = store
            .append_assistant_message(
                id,
                parent,
                &text,
                pending_reasoning,
                text_segments,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
                &shuvarie_llm::TokenUsage::default(),
            )
            .await
        {
            session.lock().await.leaf_id = Some(msg.id);
        }
    }
    let mut g = session.lock().await;
    g.push_assistant(text);
    let seq = g.messages.len() - 1;
    if !pending_reasoning.is_empty() {
        g.reasoning.insert(seq as u64, pending_reasoning.to_vec());
    }
    if !text_segments.is_empty() {
        g.text_segments.insert(seq as u64, text_segments.to_vec());
    }
    g.interrupted.insert(seq as u64, true);
}

async fn persist(
    config: &Config,
    connections: &Connections,
    config_path: Option<&Path>,
    connections_path: Option<&Path>,
    event_tx: &Sender<Event>,
) {
    let config_result = match config_path {
        Some(path) => config.save_to(path),
        None => config.save(),
    };
    let connections_result = match connections_path {
        Some(path) => connections.save_to(path),
        None => connections.save(),
    };
    match (config_result, connections_result) {
        (Ok(()), Ok(())) => {
            let _ = event_tx.send(Event::ConfigSaved).await;
        }
        (Err(e), _) | (_, Err(e)) => {
            let _ = event_tx
                .send(Event::ConfigError {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

/// Build a [`ContextBudget`] for the active model from the catalog's known
/// context length (or the config's fallback) and the `[context]` settings.
/// Returns `None` when context management is disabled.
fn context_budget(
    config: &Config,
    provider: Option<&selune::Provider>,
    model: &str,
) -> Option<shuvarie_llm::ContextBudget> {
    if config.context.disabled {
        return None;
    }
    let context_length = provider
        .and_then(|p| crate::catalog::context_length(p, model))
        .map(|n| n.max(0) as u64)
        .unwrap_or(config.context.fallback_context_length);
    Some(
        shuvarie_llm::ContextBudget::new(context_length, config.context.reserved)
            .with_keep_recent_tokens(config.context.keep_recent_tokens)
            .with_tool_output_max_chars(config.context.tool_output_max_chars),
    )
}
