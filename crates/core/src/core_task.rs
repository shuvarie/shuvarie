use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use shuvarie_db::{LockAcquire, SESSION_LOCK_HEARTBEAT_MS, Store};
use shuvarie_llm::{DeviceCodeHandler, FileChange, ProviderClient, TokenUsage};

use crate::command::Command;
use crate::embeddings::{self, EmbeddingSetup};
use crate::event::Event;
use crate::permissions::{Access, DenyCut, PermissionAnswer, PermissionGate, PermissionRequest};
use crate::question::{AnswerResponse, QuestionGate, QuestionRequest};
use crate::session::Session;
use crate::shell::Shell;
use shuvarie_config::Config;
use shuvarie_config::{Connections, ProviderConfig, TrustGrants};

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

/// Shared steering-preemption state between the run loop and the active
/// stream task. The run loop sets `ARMED` when a prompt is steered while this
/// stream is busy; the stream task flips it to `FINALIZING` when it cuts the
/// stream at an action boundary, so a concurrent `CancelStream` will not
/// abort it mid-persist. SeqCst ordering makes the two-sided race (cancel vs.
/// cut) linearizable: whichever side observes the other's state acts on it.
#[derive(Debug, Clone, Default)]
struct SteerSignal(Arc<AtomicU8>);

const STEER_IDLE: u8 = 0;
const STEER_ARMED: u8 = 1;
const STEER_FINALIZING: u8 = 2;

impl SteerSignal {
    /// Arm preemption. A no-op while the stream task is already finalizing a
    /// cut, so a steer racing the cut cannot un-guard `CancelStream`.
    fn arm(&self) {
        if self.0.load(Ordering::SeqCst) != STEER_FINALIZING {
            self.0.store(STEER_ARMED, Ordering::SeqCst);
        }
    }

    /// Disarm preemption. A no-op while the stream task is finalizing (its
    /// outcome processing resets the signal afterwards).
    fn disarm(&self) {
        if self.0.load(Ordering::SeqCst) != STEER_FINALIZING {
            self.0.store(STEER_IDLE, Ordering::SeqCst);
        }
    }

    /// Unconditionally back to idle — only valid once the stream task has
    /// ended (outcome processing or a fresh turn replacing it).
    fn reset(&self) {
        self.0.store(STEER_IDLE, Ordering::SeqCst);
    }

    fn is_armed(&self) -> bool {
        self.0.load(Ordering::SeqCst) == STEER_ARMED
    }

    /// The stream task claims the cut. Succeeds exactly once, from `ARMED`.
    fn begin_preempt(&self) -> bool {
        self.0
            .compare_exchange(
                STEER_ARMED,
                STEER_FINALIZING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    fn is_finalizing(&self) -> bool {
        self.0.load(Ordering::SeqCst) == STEER_FINALIZING
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
/// Default), and — once the session has messages — for scenes without an
/// interlude: the injected prompt is what tells the model the scene changed,
/// so an interlude-less scene can only start a session. A switch before the
/// first message picks the scene the session will start under (recording it
/// even when no session exists yet). Persists the new value and reports
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
        None => false,
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
        return Some(match name {
            Some(name) => {
                format!("scene `{name}` has no interlude; it cannot be entered mid-session")
            }
            None => format!(
                "the built-in {} scene has no interlude; it cannot be entered mid-session",
                crate::scenes::DEFAULT_SCENE_NAME
            ),
        });
    }
    let session_id = s.lock().await.id;
    {
        let mut guard = s.lock().await;
        guard.scene = name.clone();
    }
    if let Some(session_id) = session_id
        && let Err(e) = ctx.store.set_scene(session_id, name.as_deref()).await
    {
        return Some(format!("failed to persist the scene: {e}"));
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
    // what Alt+Up recalls.
    let mut steered: Vec<String> = Vec::new();
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
            switchable: false,
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
                    Command::SendMessage { content } => {
                        if is_busy(&ctx, pending_retry.as_ref()) {
                            steered.push(content.clone());
                            ctx.steer.arm();
                            let _ = ctx.event_tx.send(Event::PromptSteered { content }).await;
                            continue;
                        }
                        overflow_retries = 0;
                        pending_retry = None;
                        conn_retries = 0;
                        ctx.active_stream = None;
                        ctx.start_user_turn(content, false).await;
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
                                let content = steered.remove(0);
                                ctx.active_stream = None;
                                ctx.start_user_turn(content, true).await;
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
                                    ctx.self_replay_send(content, true).await;
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
                    }
                    Command::RecallSteered { stacked } => {
                        let content = steered.pop();
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
                            let content = steered.remove(0);
                            ctx.start_user_turn(content, true).await;
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

fn title_for(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        "Untitled session".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
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
    /// user prompt in order.
    async fn start_user_turn(&mut self, content: String, steered: bool) {
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
                let title = title_for(&content);
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
                        // Draft the session title with the provider's default
                        // small model in the background: the provisional
                        // `title_for` heuristic stands until the generated
                        // title arrives, and the compare-and-swap write
                        // upgrades it only while no manual rename has landed
                        // meanwhile. Fire-and-forget — a failed or empty
                        // generation keeps the provisional title.
                        if let Some((name, model)) = crate::title::small_model(&self.connections)
                            && let Ok(client) = client_for(
                                &mut self.clients,
                                &mut self.connections,
                                &self.event_tx,
                                &name,
                            )
                            .cloned()
                        {
                            let mut store = self.store.clone();
                            let event_tx = self.event_tx.clone();
                            let session = s.clone();
                            let provisional = title;
                            let prompt = content.clone();
                            tokio::spawn(async move {
                                let Some(generated) =
                                    crate::title::generate(&client, &model, &prompt).await
                                else {
                                    return;
                                };
                                if let Ok(true) =
                                    store.set_title_if(id, &provisional, &generated).await
                                {
                                    // Only when the in-memory title is still
                                    // the provisional one: a manual rename
                                    // that landed after the swap wins instead.
                                    let mut guard = session.lock().await;
                                    if guard.title.as_deref() == Some(provisional.as_str()) {
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
        self.self_replay_send(content, false).await;
    }

    /// Build a stream for the given user content and spawn the event-forwarding
    /// task. When `push_user` is set, the content is first appended as a user
    /// message (used by `SendMessage`); otherwise it is re-sent as-is (used by
    /// replay/resume).
    async fn self_replay_send(&mut self, content: String, push_user: bool) {
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
        let (prior, todo_records, stored_scene) = {
            let guard = s.lock().await;
            (
                guard.history_for_send(),
                guard.tool_records.clone(),
                guard.scene.clone(),
            )
        };
        let scene = crate::scenes::Scene::resolve(&self.scenes, stored_scene.as_deref());
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
        let web_search = self
            .config
            .tools
            .web_search
            .as_ref()
            .filter(|cfg| cfg.enabled);
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
        let prior = crate::scenes::inject_history(&scene, &prior, Some(&content));
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
            self.self_replay_send(content, false).await;
        }
    }
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
async fn clear_steered(steered: &mut Vec<String>, steer: &SteerSignal, event_tx: &Sender<Event>) {
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

/// The device-code prompt handler for the OAuth-backed providers (ChatGPT,
/// Copilot): forwards each sign-in prompt to the TUI as
/// [`Event::AuthPrompt`]. `try_send` because the callback runs deep inside
/// the streaming stack; a full event channel drops the prompt and the flow
/// simply times out later. `None` for every other transport.
pub(crate) fn device_code_handler(
    kind: selune::ProviderType,
    provider: String,
    event_tx: &Sender<Event>,
) -> Option<DeviceCodeHandler> {
    if !matches!(
        kind,
        selune::ProviderType::Chatgpt | selune::ProviderType::Copilot
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use selune::ProviderType;
    use shuvarie_llm::StreamItem;
    use shuvarie_llm::TokenUsage;

    #[test]
    fn device_code_handler_forwards_prompts_for_oauth_backed_kinds_only() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
        let handler = device_code_handler(ProviderType::Chatgpt, "ChatGPT".into(), &tx)
            .expect("chatgpt gets a handler");
        handler(shuvarie_llm::DeviceCodePrompt {
            verification_uri: "https://auth.openai.com/codex/device".into(),
            user_code: "ABCD-1234".into(),
        });
        match rx.try_recv().expect("prompt forwarded") {
            Event::AuthPrompt {
                provider,
                verification_uri,
                user_code,
            } => {
                assert_eq!(provider, "ChatGPT");
                assert_eq!(verification_uri, "https://auth.openai.com/codex/device");
                assert_eq!(user_code, "ABCD-1234");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(
            device_code_handler(ProviderType::Copilot, "copilot".into(), &tx).is_some(),
            "copilot gets a handler"
        );
        assert!(
            device_code_handler(ProviderType::Openai, "openai".into(), &tx).is_none(),
            "api-key transports need no device-code handler"
        );
    }

    #[tokio::test]
    async fn auth_provider_login_reports_unknown_provider() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
        handle_auth_provider_login("ghost".into(), &BTreeMap::new(), &HashMap::new(), &tx).await;
        match rx.try_recv().expect("failure reported") {
            Event::AuthFailed { provider, error } => {
                assert_eq!(provider, "ghost");
                assert!(error.contains("unknown provider"), "{error}");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_provider_login_with_a_static_key_succeeds_off_loop() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
        let mut providers = BTreeMap::new();
        providers.insert(
            "ChatGPT".to_string(),
            ProviderConfig::new("ChatGPT", "chatgpt", Some("tok".into()), None),
        );
        handle_auth_provider_login("ChatGPT".into(), &providers, &HashMap::new(), &tx).await;
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("event within timeout")
            .expect("channel open");
        match event {
            Event::AuthSuccess { provider } => assert_eq!(provider, "ChatGPT"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn auth_provider_login_on_api_key_transport_reports_failure() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
        let mut providers = BTreeMap::new();
        providers.insert(
            "OpenAI".to_string(),
            ProviderConfig::new("OpenAI", "openai", Some("sk-x".into()), None),
        );
        handle_auth_provider_login("OpenAI".into(), &providers, &HashMap::new(), &tx).await;
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("event within timeout")
            .expect("channel open");
        match event {
            Event::AuthFailed { provider, error } => {
                assert_eq!(provider, "OpenAI");
                assert!(error.contains("OAuth sign-in"), "{error}");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn retry_schedule_escalates_and_caps() {
        assert_eq!(RetrySchedule::delay_secs(1), 3);
        assert_eq!(RetrySchedule::delay_secs(2), 5);
        assert_eq!(RetrySchedule::delay_secs(3), 10);
        assert_eq!(RetrySchedule::delay_secs(4), 20);
        assert_eq!(RetrySchedule::delay_secs(5), 30);
        assert_eq!(RetrySchedule::delay_secs(6), 60);
        assert_eq!(RetrySchedule::delay_secs(10), 60);
    }

    #[test]
    fn next_connection_retry_caps_at_max() {
        assert_eq!(next_connection_retry(0, 10), Some((1, 3)));
        assert_eq!(next_connection_retry(1, 10), Some((2, 5)));
        assert_eq!(next_connection_retry(4, 10), Some((5, 30)));
        assert_eq!(next_connection_retry(5, 10), Some((6, 60)));
        assert_eq!(next_connection_retry(9, 10), Some((10, 60)));
        assert_eq!(next_connection_retry(10, 10), None);
        assert_eq!(next_connection_retry(11, 10), None);
        assert_eq!(next_connection_retry(0, 0), None, "0 = no retry");
        assert_eq!(next_connection_retry(0, 1), Some((1, 3)));
        assert_eq!(next_connection_retry(1, 1), None);
    }

    #[tokio::test]
    async fn stream_events_forward_and_accumulate_usage() {
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::Delta {
                text: "hello ".into(),
            },
            StreamItem::Usage {
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                    ..TokenUsage::default()
                },
                worker: None,
            },
            StreamItem::Delta {
                text: "world".into(),
            },
            StreamItem::Done {
                text: "hello world".into(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                    ..TokenUsage::default()
                },
            },
        ]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });

        let mut deltas = String::new();
        let mut saw_done = false;
        let mut saw_live_usage = false;
        let mut snapshot = None;
        for _ in 0..8 {
            match event_rx.recv().await {
                Some(Event::TokenReceived { content }) => deltas.push_str(&content),
                Some(Event::StreamDone { .. }) => saw_done = true,
                Some(Event::UsageUpdate { usage, .. }) => {
                    assert_eq!(usage.total_tokens, 30, "per-request usage passes through");
                    saw_live_usage = true;
                }
                Some(Event::UsageSnapshot { usage, cost }) => {
                    snapshot = Some((usage, cost));
                }
                Some(_) => {}
                None => break,
            }
            if saw_done && snapshot.is_some() {
                break;
            }
        }
        assert_eq!(deltas, "hello world");
        assert!(
            saw_done && saw_live_usage,
            "live usage and snapshot both emitted"
        );
        let (snapshot_usage, snapshot_cost) = snapshot.expect("UsageSnapshot after the turn");
        assert_eq!(snapshot_usage.total_tokens, 30, "snapshot is session-total");
        assert_eq!(snapshot_usage.input_tokens, 10);
        assert_eq!(snapshot_usage.output_tokens, 20);
        assert_eq!(snapshot_cost, 0.0, "no catalog provider, zero cost");
        let guard = session.lock().await;
        assert_eq!(guard.messages.len(), 1);
        assert_eq!(guard.messages[0].content, "hello world");
        assert_eq!(guard.tokens, 30);
        assert_eq!(guard.input_tokens, 10);
        assert_eq!(guard.output_tokens, 20);
    }

    #[tokio::test]
    async fn stream_error_forwards_and_leaves_session_clean() {
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::Delta {
                text: "partial".into(),
            },
            StreamItem::Error {
                message: "boom".into(),
            },
        ]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });

        let mut saw_error = false;
        for _ in 0..4 {
            match event_rx.recv().await {
                Some(Event::StreamError { error }) if error == "boom" => {
                    saw_error = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_error, "expected StreamError");
        let guard = session.lock().await;
        assert!(guard.messages.is_empty(), "no assistant message on error");
    }

    #[tokio::test]
    async fn stream_done_waits_for_queued_worker_items_to_drain() {
        // Mirrors the production merge shape (`select_all`, main stream
        // first): a worker's `ToolResult` queued behind an immediately-ready
        // main stream must still be delivered before `Event::StreamDone`, so
        // the TUI finishes every tool block before committing the turn.
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let done = StreamItem::Done {
            text: "final".into(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                ..TokenUsage::default()
            },
        };
        let worker_result = StreamItem::ToolResult {
            name: "grep".into(),
            output: "found".into(),
            ok: true,
            worker: Some("explore_workspace".into()),
            file_change: None,
            streams: None,
            call_id: "w1".into(),
        };
        let worker_request_usage = StreamItem::Usage {
            usage: TokenUsage {
                input_tokens: 5,
                output_tokens: 7,
                total_tokens: 12,
                ..TokenUsage::default()
            },
            worker: Some("explore_workspace".into()),
        };
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<StreamItem>(8);
        let _ = worker_tx.send(worker_request_usage).await;
        let _ = worker_tx.send(worker_result).await;
        drop(worker_tx);
        let streams: Vec<
            std::pin::Pin<Box<dyn futures_util::stream::Stream<Item = StreamItem> + Send>>,
        > = vec![
            Box::pin(futures_util::stream::iter(vec![done])),
            Box::pin(futures_util::stream::unfold(
                worker_rx,
                |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
            )),
        ];
        let stream: shuvarie_llm::StreamStream =
            Box::pin(futures_util::stream::select_all(streams));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage {
            input_tokens: 5,
            output_tokens: 7,
            total_tokens: 12,
            ..TokenUsage::default()
        }));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });

        let mut done_seen = false;
        let mut saw_tool_result_before_done = false;
        let mut saw_live_usage = false;
        let mut saw_snapshot = false;
        loop {
            match event_rx.recv().await {
                Some(Event::ToolFinished { name, .. }) if name == "grep" => {
                    assert!(!done_seen, "ToolFinished must precede StreamDone");
                    saw_tool_result_before_done = true;
                }
                Some(Event::StreamDone { .. }) => done_seen = true,
                Some(Event::UsageUpdate { usage, .. }) => {
                    assert_eq!(
                        usage.total_tokens, 12,
                        "worker request usage passes through"
                    );
                    saw_live_usage = true;
                }
                Some(Event::UsageSnapshot { usage, .. }) => {
                    assert_eq!(usage.input_tokens, 15, "snapshot combines manager + worker");
                    saw_snapshot = true;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_tool_result_before_done);
        assert!(done_seen, "StreamDone still emitted after the drain");
        assert!(saw_live_usage, "worker request emitted live usage");
        assert!(saw_snapshot, "turn end emitted UsageSnapshot");
        assert!(stream_done_rx.recv().await.is_some(), "outcome still sent");
        let guard = session.lock().await;
        assert_eq!(guard.tokens, 42, "session accumulates combined usage");
    }

    #[tokio::test]
    async fn worker_events_forward_and_usage_accumulates() {
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::WorkerStart {
                name: "explore_workspace".into(),
                args: serde_json::json!({ "task": "find the bug" }),
                call_id: "w1".into(),
            },
            StreamItem::ToolStart {
                name: "grep".into(),
                args: serde_json::json!({ "pattern": "bug" }),
                worker: Some("explore_workspace".into()),
                call_id: "t1".into(),
            },
            StreamItem::ToolResult {
                name: "grep".into(),
                output: "found".into(),
                ok: true,
                worker: Some("explore_workspace".into()),
                file_change: None,
                streams: None,
                call_id: "t1".into(),
            },
            StreamItem::WorkerResult {
                name: "explore_workspace".into(),
                output: "summary".into(),
                ok: true,
                call_id: "w1".into(),
            },
            StreamItem::Usage {
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                    ..TokenUsage::default()
                },
                worker: None,
            },
            StreamItem::Done {
                text: "done".into(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                    ..TokenUsage::default()
                },
            },
        ]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage {
            input_tokens: 5,
            output_tokens: 7,
            total_tokens: 12,
            ..TokenUsage::default()
        }));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });

        let mut saw_worker_start = false;
        let mut saw_worker_tool = false;
        let mut saw_worker_finish = false;
        let mut saw_live_usage = false;
        let mut saw_snapshot = false;
        loop {
            match event_rx.recv().await {
                Some(Event::WorkerStarted { name, .. }) if name == "explore_workspace" => {
                    saw_worker_start = true;
                }
                Some(Event::ToolStarted { worker, .. })
                    if worker.as_deref() == Some("explore_workspace") =>
                {
                    saw_worker_tool = true;
                }
                Some(Event::WorkerFinished { name, ok, .. }) if name == "explore_workspace" => {
                    saw_worker_finish = ok;
                }
                Some(Event::UsageUpdate {
                    usage,
                    context_tokens,
                    ..
                }) => {
                    assert_eq!(
                        usage.total_tokens, 30,
                        "manager request usage passes through"
                    );
                    assert_eq!(
                        context_tokens,
                        Some(30),
                        "main-stream usage carries its context footprint"
                    );
                    saw_live_usage = true;
                }
                Some(Event::UsageSnapshot { usage, .. }) => {
                    assert_eq!(usage.input_tokens, 15, "manager + worker input");
                    assert_eq!(usage.output_tokens, 27, "manager + worker output");
                    assert_eq!(usage.total_tokens, 42, "manager + worker total");
                    saw_snapshot = true;
                }
                Some(Event::StreamDone { .. }) => {}
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_worker_start, "expected WorkerStarted");
        assert!(saw_worker_tool, "expected nested tool event");
        assert!(saw_worker_finish, "expected WorkerFinished");
        assert!(saw_live_usage, "expected live UsageUpdate");
        assert!(saw_snapshot, "expected turn-end UsageSnapshot");
        let guard = session.lock().await;
        assert_eq!(guard.tokens, 42, "session accumulates combined usage");
        assert_eq!(guard.input_tokens, 15);
        assert_eq!(guard.output_tokens, 27);
    }

    #[tokio::test]
    async fn overflow_without_plan_reports_error_without_compaction_events() {
        // A session with no persisted id has nothing compactable: the overflow
        // path must still emit the budget StreamError and the Overflowed
        // outcome, but no CompactionStarted/Finished pair (there is no
        // summarizer call to bracket).
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream =
            Box::pin(futures_util::stream::iter(vec![StreamItem::Overflow]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });

        let mut budget_error = false;
        let mut unexpected = false;
        loop {
            match event_rx.recv().await {
                Some(Event::StreamError { error }) => {
                    assert!(error.contains("context budget exceeded"));
                    budget_error = true;
                }
                Some(Event::CompactionStarted) | Some(Event::CompactionFinished) => {
                    unexpected = true;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(budget_error, "overflow must report the budget error");
        assert!(
            !unexpected,
            "no compaction events without a compaction plan"
        );
        assert_eq!(
            stream_done_rx.recv().await,
            Some(StreamOutcome::Overflowed { compacted: false }),
            "uncompacted overflow outcome"
        );
    }

    /// Spawn the stream task over a fixed item list with a store-backed
    /// session row, mirroring production where the session row exists before
    /// the stream starts.
    async fn spawn_preempt_stream(
        items: Vec<StreamItem>,
        steer: SteerSignal,
        deny_cut: crate::permissions::DenyCut,
    ) -> (
        Arc<Mutex<Session>>,
        tokio::sync::mpsc::Receiver<Event>,
        tokio::sync::mpsc::Receiver<StreamOutcome>,
    ) {
        spawn_stream_core(Box::pin(futures_util::stream::iter(items)), steer, deny_cut).await
    }

    async fn spawn_stream_core(
        stream: shuvarie_llm::StreamStream,
        steer: SteerSignal,
        deny_cut: crate::permissions::DenyCut,
    ) -> (
        Arc<Mutex<Session>>,
        tokio::sync::mpsc::Receiver<Event>,
        tokio::sync::mpsc::Receiver<StreamOutcome>,
    ) {
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<Event>(64);
        let mut store = Store::open_in_memory().await.unwrap();
        let id = store
            .create_session("preempt", None, None, None)
            .await
            .unwrap();
        session.lock().await.id = Some(id);
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, stream_done_rx) = tokio::sync::mpsc::channel(1);
        let session_shared = session.clone();
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                steer,
                deny_cut,
            )
            .await;
        });
        (session, event_rx, stream_done_rx)
    }

    fn main_tool_start(name: &str, call_id: &str) -> StreamItem {
        StreamItem::ToolStart {
            name: name.into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: call_id.into(),
        }
    }

    fn main_tool_result(name: &str, call_id: &str) -> StreamItem {
        StreamItem::ToolResult {
            name: name.into(),
            output: "ok".into(),
            ok: true,
            worker: None,
            file_change: None,
            streams: None,
            call_id: call_id.into(),
        }
    }

    fn worker_tool_result(call_id: &str) -> StreamItem {
        StreamItem::ToolResult {
            name: "grep".into(),
            output: "found".into(),
            ok: true,
            worker: Some("explore_workspace".into()),
            file_change: None,
            streams: None,
            call_id: call_id.into(),
        }
    }
    #[tokio::test]
    async fn steered_prompt_cuts_after_text_segment_before_tools_run() {
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::Delta {
                    text: "plan".into(),
                },
                main_tool_start("read_file", "c1"),
                main_tool_result("read_file", "c1"),
                StreamItem::Done {
                    text: "plan".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut saw_cancel = false;
        let mut saw_tool = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::ToolStarted { .. } => saw_tool = true,
                Event::StreamCancelled => {
                    saw_cancel = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_cancel, "cut emitted StreamCancelled");
        assert!(
            !saw_tool,
            "the committed tool call must never execute once steered"
        );
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        let guard = session.lock().await;
        assert_eq!(
            guard.messages.last().map(|m| m.content.as_str()),
            Some("plan"),
            "partial text persisted as the interrupted assistant message"
        );
        assert!(guard.tool_records.is_empty(), "no tool ran");
    }

    #[tokio::test]
    async fn steered_prompt_cuts_after_thinking_segment() {
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::Reasoning {
                    text: "thinking it through".into(),
                },
                StreamItem::Delta {
                    text: "the answer".into(),
                },
                StreamItem::Done {
                    text: "the answer".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut saw_cancel = false;
        let mut saw_token = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::TokenReceived { .. } => saw_token = true,
                Event::StreamCancelled => {
                    saw_cancel = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_cancel);
        assert!(
            !saw_token,
            "text following the thinking segment must never stream"
        );
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        let guard = session.lock().await;
        assert_eq!(guard.messages.len(), 1);
        assert_eq!(guard.messages[0].content, "");
        assert_eq!(guard.interrupted.get(&0), Some(&true));
        let reasoning = guard.reasoning.get(&0).expect("reasoning persisted");
        assert_eq!(reasoning.len(), 1);
        assert_eq!(reasoning[0].text, "thinking it through");
    }

    #[tokio::test]
    async fn steered_prompt_waits_for_full_turn_without_boundary() {
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::Delta { text: "hi".into() },
                StreamItem::Done {
                    text: "hi".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut saw_done = false;
        while let Some(event) = event_rx.recv().await {
            if matches!(event, Event::StreamDone { .. }) {
                saw_done = true;
            }
        }
        assert!(saw_done, "no boundary crossed: the turn completes normally");
        assert_eq!(
            stream_done_rx.recv().await,
            Some(StreamOutcome::Finished),
            "armed steering must not preempt a turn that never crosses a boundary"
        );
        let guard = session.lock().await;
        assert!(guard.interrupted.is_empty(), "turn committed cleanly");
    }

    #[tokio::test]
    async fn steered_prompt_ignores_worker_activity() {
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::WorkerStart {
                    name: "explore_workspace".into(),
                    args: serde_json::json!({}),
                    call_id: "w1".into(),
                },
                StreamItem::ToolStart {
                    name: "grep".into(),
                    args: serde_json::json!({}),
                    worker: Some("explore_workspace".into()),
                    call_id: "t1".into(),
                },
                worker_tool_result("t1"),
                StreamItem::Delta {
                    text: "straggler-era delta".into(),
                },
                StreamItem::Done {
                    text: "x".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        // The worker's internal tool call is not the batch settling (the
        // worker tool itself is still running), so no cut may fire — the
        // following text delta streams and the turn completes normally.
        let mut saw_token = false;
        let mut saw_cancel = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::TokenReceived { .. } => saw_token = true,
                Event::StreamCancelled => saw_cancel = true,
                _ => {}
            }
        }
        assert!(saw_token, "worker activity must not trigger the cut");
        assert!(!saw_cancel);
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
        assert!(
            session.lock().await.interrupted.is_empty(),
            "the turn committed cleanly"
        );
    }

    #[tokio::test]
    async fn steered_prompt_cuts_when_worker_batch_settles() {
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::WorkerStart {
                    name: "explore_workspace".into(),
                    args: serde_json::json!({}),
                    call_id: "w1".into(),
                },
                StreamItem::ToolStart {
                    name: "grep".into(),
                    args: serde_json::json!({}),
                    worker: Some("explore_workspace".into()),
                    call_id: "t1".into(),
                },
                worker_tool_result("t1"),
                StreamItem::WorkerResult {
                    name: "explore_workspace".into(),
                    output: "summary".into(),
                    ok: true,
                    call_id: "w1".into(),
                },
                StreamItem::Delta {
                    text: "next round".into(),
                },
                StreamItem::Done {
                    text: "next round".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut saw_worker_finish = false;
        let mut saw_cancel = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::WorkerFinished { .. } => saw_worker_finish = true,
                Event::StreamCancelled => {
                    saw_cancel = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_worker_finish);
        assert!(saw_cancel, "cut fires after the worker batch settles");
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        let guard = session.lock().await;
        assert_eq!(
            guard.tool_records.len(),
            1,
            "the worker's internal tool call stays persisted"
        );
        assert_eq!(
            guard.tool_records[0].worker.as_deref(),
            Some("explore_workspace")
        );
    }

    #[tokio::test]
    async fn steered_prompt_cuts_only_after_whole_multi_tool_batch() {
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                main_tool_start("read_file", "c1"),
                main_tool_start("read_file", "c2"),
                main_tool_result("read_file", "c1"),
                main_tool_result("read_file", "c2"),
                StreamItem::Delta {
                    text: "next round".into(),
                },
                StreamItem::Done {
                    text: "next round".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut tool_finishes = 0;
        let mut saw_cancel = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::ToolFinished { .. } => tool_finishes += 1,
                Event::StreamCancelled => {
                    saw_cancel = true;
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(
            tool_finishes, 2,
            "both batch results surface before the cut"
        );
        assert!(saw_cancel, "cut fires once the whole batch settled");
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        assert_eq!(session.lock().await.tool_records.len(), 2);
    }

    #[tokio::test]
    async fn permission_deny_cuts_the_turn_after_the_denied_result() {
        let deny_cut = DenyCut::default();
        let wait_cut = deny_cut.clone();
        let stream: shuvarie_llm::StreamStream = Box::pin(
            futures_util::stream::iter(vec![
                main_tool_start("read_file", "c1"),
                StreamItem::ToolResult {
                    name: "read_file".into(),
                    output: "permission denied: paths: deny \".env\"".into(),
                    ok: false,
                    worker: None,
                    file_change: None,
                    streams: None,
                    call_id: "c1".into(),
                },
                StreamItem::Done {
                    text: "the model keeps going".into(),
                    usage: TokenUsage::default(),
                },
            ])
            .enumerate()
            .then(move |(i, item)| {
                let wait_cut = wait_cut.clone();
                async move {
                    // The tool denies while its call runs: the producer only
                    // yields the result once the denial happened.
                    if i == 1 {
                        while !wait_cut.is_set() {
                            tokio::task::yield_now().await;
                        }
                    }
                    item
                }
            }),
        );
        let (session, mut event_rx, mut stream_done_rx) =
            spawn_stream_core(stream, SteerSignal::default(), deny_cut.clone()).await;

        // The tool denies while its call runs: after the start, before the
        // result is streamed.
        let mut saw_start = false;
        let denied;
        loop {
            let event = event_rx.recv().await.expect("events before the cut");
            match event {
                Event::ToolStarted { .. } => {
                    saw_start = true;
                    deny_cut.trigger();
                }
                Event::ToolFinished { ok, output, .. } => {
                    denied = Some((ok, output));
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_start);
        let (ok, output) = denied.expect("the denied call finished before the cut");
        assert!(!ok, "the denial is a failed result");
        assert!(
            output.contains("permission denied"),
            "the denial reason stays visible: {output}"
        );

        let mut saw_cancel = false;
        while let Some(event) = event_rx.recv().await {
            if matches!(event, Event::StreamCancelled) {
                saw_cancel = true;
                break;
            }
        }
        assert!(saw_cancel, "the denial cut emits StreamCancelled");
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        assert!(!deny_cut.is_set(), "the stream task consumed the signal");

        let guard = session.lock().await;
        assert_eq!(guard.interrupted.get(&0), Some(&true));
        assert_eq!(guard.tool_records.len(), 1);
        let record = &guard.tool_records[0];
        assert!(
            !record.killed,
            "the denied call returned, so it is not killed"
        );
        assert!(!record.ok);
        assert!(
            record.output.contains("permission denied"),
            "{:#}",
            record.output
        );
    }

    #[tokio::test]
    async fn deny_after_done_cannot_discard_a_finished_turn() {
        let deny_cut = DenyCut::default();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                main_tool_start("grep", "c1"),
                main_tool_result("grep", "c1"),
                StreamItem::Done {
                    text: "the reply".into(),
                    usage: TokenUsage::default(),
                },
            ],
            SteerSignal::default(),
            deny_cut.clone(),
        )
        .await;

        let mut saw_done = false;
        let mut saw_cancel = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::StreamDone { .. } => {
                    saw_done = true;
                    deny_cut.trigger();
                }
                Event::StreamCancelled => {
                    saw_cancel = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_done, "the finished turn committed");
        assert!(
            !saw_cancel,
            "a straggling denial cannot cut a finished turn"
        );
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
        let guard = session.lock().await;
        assert!(guard.interrupted.is_empty());
        assert_eq!(
            guard.messages.last().map(|m| m.content.as_str()),
            Some("the reply")
        );
    }

    #[tokio::test]
    async fn steered_prompt_waits_for_thinking_to_complete() {
        // Mid-segment deltas continue the running thinking action: the cut
        // fires only once the segment completed (the text action starting),
        // and the text delta is consumed without streaming.
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::Reasoning {
                    text: "first burst".into(),
                },
                StreamItem::Reasoning {
                    text: " — second burst".into(),
                },
                StreamItem::Delta {
                    text: "the answer".into(),
                },
                StreamItem::Done {
                    text: "the answer".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut reasoning_chunks = 0;
        let mut saw_token = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::ReasoningReceived { .. } => reasoning_chunks += 1,
                Event::TokenReceived { .. } => saw_token = true,
                Event::StreamCancelled => break,
                _ => {}
            }
        }
        assert_eq!(
            reasoning_chunks, 2,
            "both thinking deltas stream before the cut"
        );
        assert!(!saw_token, "the text action never streams once steered");
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        let guard = session.lock().await;
        assert_eq!(guard.interrupted.get(&0), Some(&true));
        let reasoning = guard.reasoning.get(&0).expect("reasoning persisted");
        assert_eq!(reasoning.len(), 1, "both bursts append into one segment");
        assert_eq!(reasoning[0].text, "first burst — second burst");
    }

    #[tokio::test]
    async fn steered_prompt_waits_for_text_to_complete() {
        // Steering while the model writes its answer waits for that segment to
        // complete; with no further action the turn finishes normally and the
        // prompt is dispatched at turn end (committed cleanly).
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::Delta { text: "one".into() },
                StreamItem::Delta {
                    text: " two".into(),
                },
                StreamItem::Done {
                    text: "one two".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut tokens = 0;
        while let Some(event) = event_rx.recv().await {
            if let Event::TokenReceived { .. } = event {
                tokens += 1;
            }
        }
        assert_eq!(tokens, 2, "the whole text segment streams");
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
        let guard = session.lock().await;
        assert!(guard.interrupted.is_empty(), "turn committed cleanly");
    }

    #[tokio::test]
    async fn steered_prompt_cuts_when_worker_straggler_result_lags() {
        // A worker's internal tool result may straggle past its
        // `WorkerResult` on the merged stream; the lagging pair must not
        // keep the main batch from settling, so the cut still fires.
        let steer = SteerSignal::default();
        steer.arm();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::WorkerStart {
                    name: "explore_workspace".into(),
                    args: serde_json::json!({}),
                    call_id: "w1".into(),
                },
                StreamItem::ToolStart {
                    name: "grep".into(),
                    args: serde_json::json!({}),
                    worker: Some("explore_workspace".into()),
                    call_id: "t1".into(),
                },
                StreamItem::WorkerResult {
                    name: "explore_workspace".into(),
                    output: "summary".into(),
                    ok: true,
                    call_id: "w1".into(),
                },
                worker_tool_result("t1"),
                StreamItem::Delta {
                    text: "next round".into(),
                },
                StreamItem::Done {
                    text: "next round".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut saw_straggler = false;
        while let Some(event) = event_rx.recv().await {
            match event {
                Event::ToolFinished { name, .. } if name == "grep" => {
                    saw_straggler = true;
                }
                Event::StreamCancelled => break,
                _ => {}
            }
        }
        assert!(
            saw_straggler,
            "the lagging worker-internal result still surfaces"
        );
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        let guard = session.lock().await;
        assert_eq!(guard.tool_records.len(), 1);
    }

    #[tokio::test]
    async fn unarmed_signal_never_cuts() {
        let steer = SteerSignal::default();
        let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
            vec![
                StreamItem::Delta { text: "a".into() },
                main_tool_start("read_file", "c1"),
                main_tool_result("read_file", "c1"),
                StreamItem::Done {
                    text: "a".into(),
                    usage: TokenUsage::default(),
                },
            ],
            steer,
            DenyCut::default(),
        )
        .await;

        let mut saw_cancel = false;
        while let Some(event) = event_rx.recv().await {
            if matches!(event, Event::StreamCancelled) {
                saw_cancel = true;
            }
        }
        assert!(!saw_cancel);
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
        assert!(session.lock().await.interrupted.is_empty());
    }

    #[tokio::test]
    async fn steered_prompt_cuts_mid_stream_when_armed_between_actions() {
        // Drives the stream from a channel so the signal can be armed after
        // the text action started, mirroring a user submitting mid-stream:
        // the cut lands at the next action boundary (before the second tool
        // call executes).
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
        let mut store = Store::open_in_memory().await.unwrap();
        let id = store
            .create_session("interleaved", None, None, None)
            .await
            .unwrap();
        session.lock().await.id = Some(id);
        let (item_tx, item_rx) = tokio::sync::mpsc::channel::<StreamItem>(8);
        let stream: shuvarie_llm::StreamStream =
            Box::pin(futures_util::stream::unfold(item_rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            }));
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
        let steer = SteerSignal::default();
        let session_shared = session.clone();
        let steer_shared = steer.clone();
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state,
                stream_done_tx,
                steer_shared,
                DenyCut::default(),
            )
            .await;
        });

        let _ = item_tx
            .send(StreamItem::Delta {
                text: "working".into(),
            })
            .await;
        match event_rx.recv().await {
            Some(Event::TokenReceived { content }) => assert_eq!(content, "working"),
            other => panic!("expected token event, got {other:?}"),
        }
        let _ = item_tx.send(main_tool_start("edit_file", "c1")).await;
        match event_rx.recv().await {
            Some(Event::ToolStarted { name, .. }) => assert_eq!(name, "edit_file"),
            other => panic!("expected tool start, got {other:?}"),
        }
        // The user steers while the tool runs; the cut must wait for the
        // batch to settle, then fire before the next segment.
        steer.arm();
        let _ = item_tx.send(main_tool_result("edit_file", "c1")).await;
        match event_rx.recv().await {
            Some(Event::ToolFinished { name, .. }) => assert_eq!(name, "edit_file"),
            other => panic!("expected tool finish, got {other:?}"),
        }
        let _ = item_tx
            .send(StreamItem::Delta {
                text: "should never stream".into(),
            })
            .await;
        match event_rx.recv().await {
            Some(Event::StreamCancelled) => {}
            other => panic!("expected cancellation at the boundary, got {other:?}"),
        }
        assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
        drop(item_tx);
        let guard = session.lock().await;
        assert_eq!(guard.messages.len(), 1);
        assert_eq!(guard.messages[0].content, "working");
        assert_eq!(guard.interrupted.get(&0), Some(&true));
        assert_eq!(guard.tool_records.len(), 1);
    }

    #[tokio::test]
    async fn interrupted_turn_persists_pending_tools_as_killed() {
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);
        let mut store = Store::open_in_memory().await.unwrap();
        let id = store.create_session("cut", None, None, None).await.unwrap();
        {
            let mut guard = session.lock().await;
            guard.id = Some(id);
            guard.push_user("go");
        }
        store
            .append_message(id, None, shuvarie_llm::Role::User, "go")
            .await
            .unwrap();
        let user_row = store.load_session(id).await.unwrap().messages[0].id;
        let assistant = store
            .append_message(id, Some(user_row), shuvarie_llm::Role::Assistant, "")
            .await
            .unwrap();

        let turn_state = Arc::new(Mutex::new(TurnState {
            assistant_message_id: Some(assistant.id),
            assistant_seq: 1,
            text_segments: vec![shuvarie_db::TextSegment {
                after_tool: 0,
                text: "partial".into(),
            }],
            tool_records: vec![crate::tool_record::ToolRecord {
                name: "read_file".into(),
                args_json: "{}".into(),
                output: "out".into(),
                stderr: String::new(),
                ok: true,
                killed: false,
                worker: None,
                message_id: assistant.id,
                message_seq: 1,
                file_change: None,
                original_content: None,
                new_content: None,
                duration_ms: 5,
            }],
            pending_tools: vec![PendingToolCall {
                call_id: "c2".into(),
                name: "run_shell".into(),
                args_json: r#"{"command":"sleep 30"}"#.into(),
                worker: None,
                started: std::time::Instant::now(),
            }],
            ..TurnState::default()
        }));

        persist_interrupted_turn(
            Some(turn_state.clone()),
            &mut store,
            &Some(session.clone()),
            &event_tx,
        )
        .await;

        let stored = store.load_session(id).await.unwrap();
        let killed: Vec<_> = stored.tool_calls.iter().filter(|tc| tc.killed).collect();
        let finished: Vec<_> = stored.tool_calls.iter().filter(|tc| !tc.killed).collect();
        assert_eq!(killed.len(), 1, "the pending call persists as killed");
        assert!(finished.is_empty(), "only the kill was written here");
        assert_eq!(killed[0].name, "run_shell");
        assert_eq!(killed[0].seq, 1, "seq continues after the finished calls");
        assert!(!killed[0].ok);
        assert_eq!(killed[0].message_id, assistant.id);

        let guard = session.lock().await;
        assert_eq!(guard.messages.len(), 2);
        assert_eq!(guard.interrupted.get(&1), Some(&true));
        assert_eq!(guard.tool_records.len(), 2);
        assert!(!guard.tool_records[0].killed);
        assert!(guard.tool_records[1].killed);
        assert_eq!(guard.tool_records[1].message_seq, 1);
        assert_eq!(guard.tool_records[1].name, "run_shell");
    }

    #[tokio::test]
    async fn tool_result_clears_the_pending_call() {
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
        let mut store = Store::open_in_memory().await.unwrap();
        let id = store
            .create_session("settle", None, None, None)
            .await
            .unwrap();
        session.lock().await.id = Some(id);
        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            main_tool_start("edit_file", "c1"),
            main_tool_result("edit_file", "c1"),
            StreamItem::Done {
                text: "done".into(),
                usage: TokenUsage::default(),
            },
        ]));
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
        let session_shared = session.clone();
        let turn_state_shared = turn_state.clone();
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state_shared,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });
        while let Some(event) = event_rx.recv().await {
            if matches!(event, Event::StreamDone { .. }) {
                break;
            }
        }
        assert!(turn_state.lock().await.pending_tools.is_empty());
        let guard = session.lock().await;
        assert_eq!(guard.tool_records.len(), 1);
        assert!(!guard.tool_records[0].killed);
    }

    #[tokio::test]
    async fn text_runs_seal_at_tool_gaps() {
        let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
        let mut store = Store::open_in_memory().await.unwrap();
        let id = store
            .create_session("runs", None, None, None)
            .await
            .unwrap();
        {
            let mut guard = session.lock().await;
            guard.id = Some(id);
            guard.push_user("go");
        }
        store
            .append_message(id, None, shuvarie_llm::Role::User, "go")
            .await
            .unwrap();
        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::Delta {
                text: "start".into(),
            },
            main_tool_start("read_file", "c1"),
            main_tool_result("read_file", "c1"),
            StreamItem::Delta {
                text: "\n\nresumed".into(),
            },
            StreamItem::Done {
                text: "start\n\nresumed".into(),
                usage: TokenUsage::default(),
            },
        ]));
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        let turn_state = Arc::new(Mutex::new(TurnState::default()));
        let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
        let session_shared = session.clone();
        let turn_state_shared = turn_state.clone();
        let store_shared = store.clone();
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                None,
                store_shared,
                "ollama-model".into(),
                20_000,
                worker_usage,
                None,
                event_tx,
                turn_state_shared,
                stream_done_tx,
                SteerSignal::default(),
                DenyCut::default(),
            )
            .await;
        });
        while let Some(event) = event_rx.recv().await {
            if matches!(event, Event::StreamDone { .. }) {
                break;
            }
        }
        let stored = store.load_session(id).await.unwrap();
        let assistant = &stored.messages[1];
        assert_eq!(assistant.content, "start\n\nresumed");
        assert_eq!(
            assistant.text_segments,
            vec![
                shuvarie_db::TextSegment {
                    after_tool: 0,
                    text: "start".into(),
                },
                shuvarie_db::TextSegment {
                    after_tool: 1,
                    text: "\n\nresumed".into(),
                },
            ],
            "the resumed run seals at the tool gap"
        );
        let guard = session.lock().await;
        assert_eq!(
            guard.text_segments.get(&1).map(Vec::as_slice),
            Some(assistant.text_segments.as_slice())
        );
    }

    async fn chain_session() -> (Store, uuid::Uuid, Vec<u64>) {
        let mut store = Store::open_in_memory().await.unwrap();
        let sid = store
            .create_session("fork", None, None, None)
            .await
            .unwrap();
        let user = store
            .append_message(sid, None, shuvarie_llm::Role::User, "one")
            .await
            .unwrap();
        let a1 = store
            .append_message(sid, Some(user.id), shuvarie_llm::Role::Assistant, "r1")
            .await
            .unwrap();
        let u2 = store
            .append_message(sid, Some(a1.id), shuvarie_llm::Role::User, "two")
            .await
            .unwrap();
        let a2 = store
            .append_message(sid, Some(u2.id), shuvarie_llm::Role::Assistant, "r2")
            .await
            .unwrap();
        store.set_active_leaf(sid, Some(a2.id)).await.unwrap();
        (store, sid, vec![user.id, a1.id, u2.id, a2.id])
    }

    #[tokio::test]
    async fn fork_session_undo_forks_before_the_last_user_prompt() {
        let (mut store, sid, ids) = chain_session().await;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let prompt = fork_session(&mut store, sid, None, false, None, &event_tx)
            .await
            .unwrap();
        assert_eq!(prompt.as_deref(), Some("two"), "the prompt is recalled");

        let stored = store.load_session(sid).await.unwrap();
        assert_eq!(stored.leaf_id, Some(ids[1]), "the tip walks back to a1");
        assert_eq!(
            stored.messages.len(),
            4,
            "forking keeps every row in the tree"
        );
    }

    #[tokio::test]
    async fn fork_session_at_an_assistant_node_forks_before_it() {
        let (mut store, sid, ids) = chain_session().await;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let prompt = fork_session(&mut store, sid, Some(ids[1]), false, None, &event_tx)
            .await
            .unwrap();
        assert_eq!(prompt.as_deref(), Some("r1"), "the reply is recalled");
        let stored = store.load_session(sid).await.unwrap();
        assert_eq!(
            stored.leaf_id,
            Some(ids[0]),
            "the tip walks back before the reply"
        );
    }

    #[tokio::test]
    async fn fork_session_at_a_user_node_forks_before_it() {
        let (mut store, sid, ids) = chain_session().await;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let prompt = fork_session(&mut store, sid, Some(ids[2]), false, None, &event_tx)
            .await
            .unwrap();
        assert_eq!(prompt.as_deref(), Some("two"), "the prompt is recalled");
        let stored = store.load_session(sid).await.unwrap();
        assert_eq!(
            stored.leaf_id,
            Some(ids[1]),
            "the tip walks back before the prompt"
        );
    }

    #[tokio::test]
    async fn fork_session_at_the_root_prompt_starts_over() {
        let (mut store, sid, ids) = chain_session().await;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let prompt = fork_session(&mut store, sid, Some(ids[0]), false, None, &event_tx)
            .await
            .unwrap();
        assert_eq!(prompt.as_deref(), Some("one"));
        let stored = store.load_session(sid).await.unwrap();
        assert_eq!(
            stored.leaf_id,
            Some(shuvarie_db::EMPTY_LEAF),
            "the active path is empty again"
        );
        assert_eq!(stored.messages.len(), 4, "every row stays in the tree");

        let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
        assert!(
            loaded.messages.is_empty(),
            "a reload of a cleared path shows an empty chat"
        );
        assert_eq!(loaded.leaf_id, None);
    }

    #[tokio::test]
    async fn fork_session_at_a_summary_node_walks_to_it() {
        let (mut store, sid, ids) = chain_session().await;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);
        let summary = store
            .append_summary(sid, Some(ids[1]), "the talk so far")
            .await
            .unwrap();

        let prompt = fork_session(&mut store, sid, Some(summary.id), false, None, &event_tx)
            .await
            .unwrap();
        assert_eq!(prompt, None, "summary markers carry no recall");
        let stored = store.load_session(sid).await.unwrap();
        assert_eq!(stored.leaf_id, Some(summary.id));
    }

    #[tokio::test]
    async fn fork_session_without_summarizer_errors_on_summarize() {
        let (mut store, sid, ids) = chain_session().await;
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let err = fork_session(&mut store, sid, Some(ids[2]), true, None, &event_tx)
            .await
            .unwrap_err();
        assert!(
            err.contains("without an active provider"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn fork_session_undo_on_a_root_only_session_starts_over() {
        let mut store = Store::open_in_memory().await.unwrap();
        let sid = store
            .create_session("solo", None, None, None)
            .await
            .unwrap();
        store
            .append_message(sid, None, shuvarie_llm::Role::User, "only")
            .await
            .unwrap();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let prompt = fork_session(&mut store, sid, None, false, None, &event_tx)
            .await
            .unwrap();
        assert_eq!(prompt.as_deref(), Some("only"));
        let stored = store.load_session(sid).await.unwrap();
        assert_eq!(
            stored.leaf_id,
            Some(shuvarie_db::EMPTY_LEAF),
            "the active path is empty again"
        );
        assert_eq!(stored.messages.len(), 1, "the root prompt survives");

        let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
        assert!(
            loaded.messages.is_empty(),
            "a reload of a cleared path shows an empty chat"
        );

        let restart = store
            .append_message(sid, None, shuvarie_llm::Role::User, "again")
            .await
            .unwrap();
        let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
        assert_eq!(
            loaded.messages.len(),
            1,
            "a turn after the undo starts a fresh root prompt"
        );
        assert_eq!(loaded.messages[0].content, "again");
        assert_eq!(loaded.leaf_id, Some(restart.id));
    }

    #[tokio::test]
    async fn delete_branch_refuses_the_active_leaf() {
        let (mut store, sid, ids) = chain_session().await;
        let stored = store.load_session(sid).await.unwrap();
        assert!(subtree_contains(&stored, ids[0], stored.leaf_id));
    }

    #[tokio::test]
    async fn shell_chunks_resolve_to_their_own_concurrent_calls() {
        // One worker runs two `run_shell` calls concurrently: each streamed
        // chunk resolves against the pending calls' args and must carry its
        // own call id. A chunk arriving after its call settled (no pending
        // candidate left) resolves to `None`.
        let items = vec![
            StreamItem::WorkerStart {
                name: "run_tests".into(),
                args: serde_json::json!({ "task": "verify" }),
                call_id: "w1".into(),
            },
            StreamItem::ToolStart {
                name: "run_shell".into(),
                args: serde_json::json!({ "command": "cargo test" }),
                worker: Some("run_tests".into()),
                call_id: "s1".into(),
            },
            StreamItem::ToolStart {
                name: "run_shell".into(),
                args: serde_json::json!({ "command": "cargo clippy" }),
                worker: Some("run_tests".into()),
                call_id: "s2".into(),
            },
            StreamItem::ShellOutput {
                worker: Some("run_tests".into()),
                command: "cargo clippy".into(),
                stdout: "clippy tail".into(),
                stderr: String::new(),
            },
            StreamItem::ShellOutput {
                worker: Some("run_tests".into()),
                command: "cargo test".into(),
                stdout: "test tail".into(),
                stderr: String::new(),
            },
            StreamItem::ToolResult {
                name: "run_shell".into(),
                output: "exit 0\ntest tail".into(),
                ok: true,
                worker: Some("run_tests".into()),
                file_change: None,
                streams: None,
                call_id: "s1".into(),
            },
            StreamItem::ShellOutput {
                worker: Some("run_tests".into()),
                command: "cargo test".into(),
                stdout: "straggler".into(),
                stderr: String::new(),
            },
            StreamItem::WorkerResult {
                name: "run_tests".into(),
                output: "done".into(),
                ok: true,
                call_id: "w1".into(),
            },
            StreamItem::Done {
                text: "done".into(),
                usage: TokenUsage::default(),
            },
        ];
        let (_session, mut event_rx, mut done_rx) =
            spawn_preempt_stream(items, SteerSignal::default(), DenyCut::default()).await;

        let mut chunks: Vec<(Option<String>, Option<String>, String)> = Vec::new();
        while let Some(event) = event_rx.recv().await {
            if let Event::ToolOutput {
                worker,
                call_id,
                stdout,
                ..
            } = event
            {
                chunks.push((worker, call_id, stdout));
            }
        }
        assert_eq!(
            done_rx.recv().await,
            Some(StreamOutcome::Finished),
            "the turn completes normally"
        );
        assert_eq!(
            chunks,
            vec![
                (
                    Some("run_tests".into()),
                    Some("s2".into()),
                    "clippy tail".into()
                ),
                (
                    Some("run_tests".into()),
                    Some("s1".into()),
                    "test tail".into()
                ),
                (Some("run_tests".into()), None, "straggler".into()),
            ],
            "each chunk routes to its own call; the settled call's chunk is unresolved"
        );
    }

    #[tokio::test]
    async fn ambiguous_shell_chunk_stays_unresolved() {
        // Two concurrent calls running the identical command cannot be told
        // apart: the chunk resolves to `None` instead of guessing.
        let items = vec![
            StreamItem::WorkerStart {
                name: "run_tests".into(),
                args: serde_json::json!({ "task": "verify" }),
                call_id: "w1".into(),
            },
            StreamItem::ToolStart {
                name: "run_shell".into(),
                args: serde_json::json!({ "command": "cargo test" }),
                worker: Some("run_tests".into()),
                call_id: "s1".into(),
            },
            StreamItem::ToolStart {
                name: "run_shell".into(),
                args: serde_json::json!({ "command": "cargo test" }),
                worker: Some("run_tests".into()),
                call_id: "s2".into(),
            },
            StreamItem::ShellOutput {
                worker: Some("run_tests".into()),
                command: "cargo test".into(),
                stdout: "tail".into(),
                stderr: String::new(),
            },
            StreamItem::Done {
                text: "done".into(),
                usage: TokenUsage::default(),
            },
        ];
        let (_session, mut event_rx, _done_rx) =
            spawn_preempt_stream(items, SteerSignal::default(), DenyCut::default()).await;

        while let Some(event) = event_rx.recv().await {
            if let Event::ToolOutput { call_id, .. } = event {
                assert_eq!(call_id, None, "identical commands stay ambiguous");
            }
        }
    }

    #[tokio::test]
    async fn merged_shell_chunks_follow_their_tool_start() {
        // The shell sub-stream polls after the LLM stream, so a call's
        // ToolStart event precedes its own chunk even when both are ready at
        // the same poll — the block the chunk streams into exists by then.
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(4);
        let _ = chunk_tx
            .send(crate::tools::ShellChunk {
                worker: Some("run_tests".into()),
                command: "cargo test".into(),
                stdout: "tail".into(),
                stderr: String::new(),
            })
            .await;
        drop(chunk_tx);
        let llm: shuvarie_llm::StreamStream =
            Box::pin(futures_util::stream::iter(vec![StreamItem::ToolStart {
                name: "run_shell".into(),
                args: serde_json::json!({ "command": "cargo test" }),
                worker: Some("run_tests".into()),
                call_id: "s1".into(),
            }]));
        let merged = merge_shell_chunks(llm, chunk_rx);

        let mut items = Vec::new();
        let mut merged = merged;
        while let Some(item) = merged.next().await {
            items.push(item);
        }
        assert_eq!(items.len(), 2, "the chunk stream ends after senders drop");
        assert!(
            matches!(&items[0], StreamItem::ToolStart { call_id, .. } if call_id == "s1"),
            "the ToolStart is polled first: {items:?}"
        );
        assert!(
            matches!(&items[1], StreamItem::ShellOutput { command, .. } if command == "cargo test"),
            "the chunk follows: {items:?}"
        );
    }
}
