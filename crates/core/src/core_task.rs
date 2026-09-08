use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use shuvarie_db::Store;
use shuvarie_llm::{FileChange, ProviderClient, TokenUsage};

use crate::command::Command;
use crate::config::Config;
use crate::config::{Connections, ProviderConfig};
use crate::embeddings::{self, EmbeddingSetup};
use crate::event::Event;
use crate::question::{AnswerResponse, QuestionGate, QuestionRequest};
use crate::session::{CONTINUE_PROMPT, Session};

/// How a streamed turn ended, reported back to the run loop so it can decide
/// whether to auto-continue after a context overflow.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamOutcome {
    /// The turn completed normally (or was cancelled/errored).
    Finished,
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
    config: Config,
    workspace_root: PathBuf,
    agents_md_context: crate::context::LoadedContext,
    skills: crate::skills::Skills,
    session: Option<Arc<Mutex<Session>>>,
    manager_turns: usize,
    worker_turns: usize,
    max_output_chars: usize,
    max_output_bytes: usize,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    connections: Connections,
    store: Store,
    startup: StartupSession,
    config_path: Option<PathBuf>,
    connections_path: Option<PathBuf>,
    mut cmd_rx: Receiver<Command>,
    event_tx: Sender<Event>,
) {
    let mut semantic_search: Option<AbortHandle> = None;
    let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel::<StreamOutcome>(1);
    let mut overflow_retries: usize = 0;
    const MAX_OVERFLOW_RETRIES: usize = 3;

    // Connection-failure auto-retry state: consecutive failures within one
    // retry chain, plus the currently scheduled wait (if any).
    let mut conn_retries: usize = 0;
    let mut pending_retry: Option<PendingRetry> = None;

    let (question_tx, mut question_rx) = tokio::sync::mpsc::channel::<QuestionRequest>(8);
    let mut pending_questions: HashMap<u64, oneshot::Sender<AnswerResponse>> = HashMap::new();
    let mut next_question_id: u64 = 0;

    let mut clients: HashMap<String, ProviderClient> = HashMap::new();
    // Refresh the provider catalog from the service (falling back to embedded)
    // before wiring up clients, so pricing/context/embedding lookups see fresh
    // data. Bounded so an unreachable catalog never blocks startup.
    let refresh = tokio::task::spawn_blocking(crate::catalog::refresh);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), refresh)
        .await
        .map(|r| r.unwrap_or_default())
        .unwrap_or_default();
    let embedding_setup = embeddings::setup(&config, &connections, &mut clients);
    if let Some(setup) = embedding_setup.clone() {
        let store_backfill = store.clone();
        tokio::spawn(async move {
            embeddings::backfill(&mut store_backfill.clone(), &setup).await;
        });
    }

    let workspace_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let agents_md_context = crate::context::load_agents_md(&workspace_root);
    let lsp_config = shuvarie_lsp::LspConfig::from(&config.lsp);
    let lsp = std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_lsp::LspManager::new(
        workspace_root.clone(),
        lsp_config.enabled,
        lsp_config.resolve(),
    )));
    let mut lsp_pump_tick = tokio::time::interval(std::time::Duration::from_millis(500));
    lsp_pump_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Don't fire immediately on the first tick.
    lsp_pump_tick.reset();

    let skills = crate::skills::Skills::load(&workspace_root, &config.skills);
    let _ = event_tx
        .send(Event::SkillsLoaded {
            skills: skills.skills.clone(),
            warnings: skills.warnings.clone(),
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
        config,
        workspace_root,
        agents_md_context,
        skills,
        session: None,
        manager_turns,
        worker_turns,
        max_output_chars,
        max_output_bytes,
    };
    load_startup_session(&mut ctx.store, &mut ctx.session, &ctx.event_tx, startup).await;

    loop {
        let retry_deadline = pending_retry.as_ref().map(|p| p.deadline);
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Command::Ping => {
                        let _ = ctx.event_tx.send(Event::Pong).await;
                    }
                    Command::ListModels { provider_name } => {
                        let client = match client_for(&mut ctx.clients, &mut ctx.connections, &provider_name) {
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
                                crate::config::Active {
                                    provider: name.clone(),
                                    model: None,
                                    variant: None,
                                }
                            });
                            active.provider = name.clone();
                            if !ctx.clients.contains_key(&name)
                                && let Some(pc) = ctx.connections.providers.get(&name)
                                && let Ok(client) = build_client(pc)
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
                    Command::StartSession => {
                        overflow_retries = 0;
                        pending_retry = None;
                        conn_retries = 0;
                        dismiss_pending_questions(&mut pending_questions);
                        ctx.session = Some(Arc::new(Mutex::new(Session::new())));
                        let _ = ctx.event_tx.send(Event::SessionStarted).await;
                    }
                    Command::NewSession => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        overflow_retries = 0;
                        pending_retry = None;
                        conn_retries = 0;
                        dismiss_pending_questions(&mut pending_questions);
                        ctx.session = Some(Arc::new(Mutex::new(Session::new())));
                        let _ = ctx.event_tx.send(Event::SessionStarted).await;
                    }
                    Command::SendMessage { content } => {
                        if ctx.active_stream.as_ref().is_some_and(|h| !h.is_finished()) {
                            let _ = ctx.event_tx
                                .send(Event::StreamError {
                                    error: "a reply is already streaming".into(),
                                })
                                .await;
                            continue;
                        }
                        overflow_retries = 0;
                        pending_retry = None;
                        conn_retries = 0;
                        ctx.active_stream = None;
                        if ctx.session.is_none() {
                            ctx.session = Some(Arc::new(Mutex::new(Session::new())));
                            let _ = ctx.event_tx.send(Event::SessionStarted).await;
                        }
                        let s = ctx.session.as_ref().unwrap();
                        s.lock().await.push_user(content.clone());

                        {
                            let mut guard = s.lock().await;
                            if guard.id.is_none() {
                                let title = title_for(&content);
                                match ctx
                                    .store
                                    .create_session(
                                        &title,
                                        ctx.connections.active.as_ref().map(|a| a.provider.as_str()),
                                        ctx.connections.active.as_ref().and_then(|a| a.model.as_deref()),
                                    )
                                    .await
                                {
                                    Ok(id) => {
                                        guard.id = Some(id);
                                        guard.title = Some(title.clone());
                                        let _ = ctx.event_tx.send(Event::SessionCreated { id, title }).await;
                                    }
                                    Err(e) => {
                                        let _ = ctx.event_tx
                                            .send(Event::StreamError {
                                                error: format!("failed to create session: {e}"),
                                            })
                                            .await;
                                        continue;
                                    }
                                }
                            }
                            let id = guard.id.unwrap();
                            let seq = guard.messages.len() - 1;
                            let msg = ctx
                                .store
                                .append_message(id, guard.messages.last().unwrap().role, &content)
                                .await;
                            match msg {
                                Ok(msg) => {
                                    if let Some(setup) = &ctx.embedding_setup {
                                        let store_idx = ctx.store.clone();
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
                                    let _ = ctx.event_tx
                                        .send(Event::StreamError {
                                            error: format!("failed to persist message: {e}"),
                                        })
                                        .await;
                                    continue;
                                }
                            }
                        }

                        ctx.self_replay_send(content, false).await;
                    }
                    Command::CancelStream => {
                        let mut aborted = false;
                        if let Some(handle) = ctx.active_stream.take()
                            && !handle.is_finished()
                        {
                            handle.abort();
                            dismiss_pending_questions(&mut pending_questions);
                            persist_interrupted_turn(
                                ctx.turn_state.take(),
                                &mut ctx.store,
                                &ctx.session,
                                &ctx.event_tx,
                            )
                            .await;
                            let _ = ctx.event_tx.send(Event::StreamCancelled).await;
                            aborted = true;
                        }
                        if !aborted
                            && let Some(pending) = pending_retry.take()
                        {
                            conn_retries = 0;
                            let _ = ctx.event_tx.send(Event::StreamError { error: pending.message }).await;
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
                        dismiss_pending_questions(&mut pending_questions);
                        match ctx.store.load_session(id).await {
                            Ok(stored) => {
                                let loaded = Session::from_stored(stored);
                                ctx.session = Some(Arc::new(Mutex::new(loaded.clone())));
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
                    Command::DeleteSession { id } => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        match ctx.store.delete_session(id).await {
                            Ok(()) => {
                                if let Some(s) = &ctx.session
                                    && s.lock().await.id == Some(id)
                                {
                                    *s.lock().await = Session::new();
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
                    Command::UndoLastTurn => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        let Some(s) = &ctx.session else { continue; };
                        let session_id = s.lock().await.id;
                        let Some(sid) = session_id else { continue; };
                        match undo_last_turn(&mut ctx.store, sid).await {
                            Ok(true) => {
                                if let Ok(stored) = ctx.store.load_session(sid).await {
                                    let loaded = Session::from_stored(stored);
                                    *s.lock().await = loaded.clone();
                                    let _ = ctx.event_tx
                                        .send(Event::TurnReverted { session: loaded })
                                        .await;
                                }
                            }
                            Ok(false) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: "nothing to undo".into(),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: format!("undo failed: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::Redo => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        let Some(s) = &ctx.session else { continue; };
                        let session_id = s.lock().await.id;
                        let Some(sid) = session_id else { continue; };
                        match redo_turn(&mut ctx.store, sid).await {
                            Ok(true) => {
                                if let Ok(stored) = ctx.store.load_session(sid).await {
                                    let loaded = Session::from_stored(stored);
                                    *s.lock().await = loaded.clone();
                                    let _ = ctx.event_tx
                                        .send(Event::TurnRestored { session: loaded })
                                        .await;
                                }
                            }
                            Ok(false) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: "nothing to redo".into(),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: format!("redo failed: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::Replay => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        let Some(s) = &ctx.session else { continue; };
                        let session_id = s.lock().await.id;
                        let Some(sid) = session_id else { continue; };
                        let last_user_content = s.lock().await.messages.iter().rev()
                            .find(|m| m.role == shuvarie_llm::Role::User)
                            .map(|m| m.content.clone());
                        match undo_last_turn(&mut ctx.store, sid).await {
                            Ok(true) => {
                                if let Ok(stored) = ctx.store.load_session(sid).await {
                                    let loaded = Session::from_stored(stored);
                                    *s.lock().await = loaded.clone();
                                    let _ = ctx.event_tx
                                        .send(Event::TurnReverted { session: loaded })
                                        .await;
                                }
                                if let Some(content) = last_user_content {
                                    ctx.self_replay_send(content, true).await;
                                }
                            }
                            Ok(false) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: "nothing to replay".into(),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let _ = ctx.event_tx
                                    .send(Event::SessionError {
                                        error: format!("replay failed: {e}"),
                                    })
                                    .await;
                            }
                        }
                    }
                    Command::Continue => {
                        if stream_busy(&ctx.active_stream, &ctx.event_tx).await {
                            continue;
                        }
                        pending_retry = None;
                        conn_retries = 0;
                        let Some(s) = &ctx.session else { continue; };
                        if !s.lock().await.can_continue() {
                            let _ = ctx
                                .event_tx
                                .send(Event::SessionError {
                                    error: "nothing to continue".into(),
                                })
                                .await;
                            continue;
                        }
                        ctx.self_replay_send(CONTINUE_PROMPT.to_string(), true)
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
            outcome = stream_done_rx.recv() => {
                let Some(outcome) = outcome else { continue };
                match outcome {
                    StreamOutcome::Finished => {
                        overflow_retries = 0;
                        conn_retries = 0;
                    }
                    StreamOutcome::Overflowed { compacted } => {
                        if compacted && overflow_retries < MAX_OVERFLOW_RETRIES {
                            overflow_retries += 1;
                            ctx.resume_last_turn().await;
                        } else {
                            overflow_retries = 0;
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

    ctx.lsp.lock().await.shutdown_all().await;
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
You can read, write, and edit files, list directories, grep for text, run commands, and fetch \
web pages with the `webfetch` tool (URLs must start with http:// or https://). \
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
    pending_text: String,
    pending_reasoning: Vec<shuvarie_db::ReasoningSegment>,
    tool_records: Vec<crate::tool_record::ToolRecord>,
}

/// A tool call whose result has not arrived yet: the serialized args plus the
/// moment the call started, so the finished call can record its duration.
struct PendingTool {
    args_json: String,
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

async fn undo_last_turn(store: &mut Store, session_id: uuid::Uuid) -> Result<bool, String> {
    let turn = store
        .last_turn(session_id)
        .await
        .map_err(|e| e.to_string())?;
    let Some((user_msg, assistant_msg)) = turn else {
        return Ok(false);
    };
    let tool_calls = store
        .tool_calls_for_message(assistant_msg.id)
        .await
        .map_err(|e| e.to_string())?;
    let usage = shuvarie_llm::TokenUsage {
        input_tokens: assistant_msg.input_tokens,
        output_tokens: assistant_msg.output_tokens,
        total_tokens: assistant_msg.total_tokens,
        cached_input_tokens: assistant_msg.cached_input_tokens,
        reasoning_tokens: assistant_msg.reasoning_tokens,
        ..Default::default()
    };
    let entry = shuvarie_db::UndoEntry {
        turn_seq: assistant_msg.seq,
        user_content: user_msg.content,
        assistant_content: assistant_msg.content,
        reasoning: assistant_msg.reasoning,
        usage,
        cost: assistant_msg.cost,
        tool_calls: tool_calls.clone(),
    };
    store
        .append_undo_log(session_id, &entry)
        .await
        .map_err(|e| e.to_string())?;
    for tc in &tool_calls {
        let change = parse_file_change(&tc.file_change_json);
        let Some(change) = change else {
            continue;
        };
        for (path, original, _new) in change.patch_files() {
            if path.is_empty() {
                continue;
            }
            if let Some(original) = original {
                let _ = std::fs::write(path, original);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    store
        .delete_tool_calls_for_message(assistant_msg.id)
        .await
        .map_err(|e| e.to_string())?;
    store
        .delete_message(assistant_msg.id)
        .await
        .map_err(|e| e.to_string())?;
    store
        .delete_message(user_msg.id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

async fn redo_turn(store: &mut Store, session_id: uuid::Uuid) -> Result<bool, String> {
    let entry = store
        .pop_undo_log(session_id)
        .await
        .map_err(|e| e.to_string())?;
    let Some(entry) = entry else {
        return Ok(false);
    };
    let _user_msg = store
        .append_message(session_id, shuvarie_llm::Role::User, &entry.user_content)
        .await
        .map_err(|e| e.to_string())?;
    let assistant_msg = store
        .append_assistant_message(
            session_id,
            &entry.assistant_content,
            &entry.reasoning,
            false,
            entry.usage,
            entry.cost,
        )
        .await
        .map_err(|e| e.to_string())?;
    for tc in &entry.tool_calls {
        if let Some(change) = parse_file_change(&tc.file_change_json) {
            for (path, _original, new) in change.patch_files() {
                if path.is_empty() {
                    continue;
                }
                if let Some(new) = new {
                    let _ = std::fs::write(path, new);
                }
            }
        }
        let (fc_json, original, new) = (
            tc.file_change_json.clone(),
            tc.original_content.clone(),
            tc.new_content.clone(),
        );
        let _ = store
            .append_tool_call(
                session_id,
                assistant_msg.id,
                tc.seq,
                &tc.name,
                &tc.args_json,
                &tc.output,
                &tc.stderr,
                tc.ok,
                tc.worker.as_deref(),
                &fc_json,
                original.as_deref(),
                new.as_deref(),
                tc.duration_ms,
            )
            .await;
    }
    Ok(true)
}

impl CoreCtx {
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
                let _ = self
                    .store
                    .append_message(id, guard.messages.last().unwrap().role, &content)
                    .await;
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
        let client = match client_for(&mut self.clients, &mut self.connections, &provider_name) {
            Ok(c) => c.clone(),
            Err(e) => {
                let _ = self.event_tx.send(Event::StreamError { error: e }).await;
                return;
            }
        };
        let (prior, todo_records): (
            Vec<shuvarie_llm::ChatMsg>,
            Vec<crate::tool_record::ToolRecord>,
        ) = {
            let guard = s.lock().await;
            (guard.history_for_send(), guard.tool_records.clone())
        };
        let todo_state = crate::tools::todos::TodoState::from_records(&todo_records);
        let loaded_context =
            self.agents_md_context
                .clone()
                .merged(crate::context::load_context_dir(
                    &self.workspace_root,
                    self.agents_md_context.remaining_budget(),
                ));
        if !loaded_context.is_empty() {
            let _ = self
                .event_tx
                .send(Event::ContextLoaded {
                    paths: loaded_context.files.clone(),
                })
                .await;
        }
        let base = match self.skills.preamble_section() {
            Some(section) => format!("{AGENT_PREAMBLE}\n\n{section}"),
            None => AGENT_PREAMBLE.to_string(),
        };
        let preamble = crate::context::build_preamble(&base, &loaded_context);
        let question_gate = QuestionGate::new(self.question_tx.clone());
        let (shell_tx, mut shell_rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(64);
        let file_locks = crate::tools::FileLocks::new();
        let tools = crate::tools::all_tools(
            self.lsp.clone(),
            file_locks.clone(),
            crate::tools::ReadCache::new(),
            self.max_output_chars,
            self.max_output_bytes,
            question_gate,
            crate::tools::ShellOutputTx::new(shell_tx.clone()),
            todo_state,
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
        let output_forward_tx = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some(chunk) = shell_rx.recv().await {
                let _ = output_forward_tx
                    .send(Event::ToolOutput {
                        tool: "run_shell".to_string(),
                        worker: chunk.worker,
                        stdout: chunk.stdout,
                        stderr: chunk.stderr,
                    })
                    .await;
            }
        });
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
        );
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
                )
                .await;
            })
            .abort_handle(),
        );
    }

    /// Delete the interrupted assistant turn (message + tool calls) and re-stream
    /// from the last user message. Used by the auto-continue path after a context
    /// overflow.
    async fn resume_last_turn(&mut self) {
        let Some(s) = &self.session else {
            return;
        };
        let (session_id, last_user_content) = {
            let guard = s.lock().await;
            let sid = guard.id;
            let last_user = guard
                .messages
                .iter()
                .rev()
                .find(|m| m.role == shuvarie_llm::Role::User)
                .map(|m| m.content.clone());
            (sid, last_user)
        };
        let Some(sid) = session_id else {
            return;
        };
        let Some(content) = last_user_content else {
            return;
        };
        if let Ok(Some((_user_msg, assistant_msg))) = self.store.last_turn(sid).await {
            let _ = self
                .store
                .delete_tool_calls_for_message(assistant_msg.id)
                .await;
            let _ = self.store.delete_message(assistant_msg.id).await;
        }
        if let Ok(stored) = self.store.load_session(sid).await {
            let loaded = Session::from_stored(stored);
            *s.lock().await = loaded.clone();
            let _ = self
                .event_tx
                .send(Event::TurnReverted { session: loaded })
                .await;
        }
        self.self_replay_send(content, false).await;
    }
}

async fn load_startup_session(
    store: &mut Store,
    session: &mut Option<Arc<Mutex<Session>>>,
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

/// Settle all pending questions as dismissed (dropping the responder makes
/// the awaiting tool error out with "The user dismissed this question").
fn dismiss_pending_questions(
    pending_questions: &mut HashMap<u64, oneshot::Sender<AnswerResponse>>,
) {
    pending_questions.clear();
}

fn client_for<'a>(
    clients: &'a mut HashMap<String, ProviderClient>,
    connections: &'a mut Connections,
    name: &str,
) -> Result<&'a ProviderClient, String> {
    if !clients.contains_key(name) {
        let pc = connections
            .providers
            .get(name)
            .ok_or_else(|| format!("provider '{name}' not found"))?;
        let client = build_client(pc)?;
        clients.insert(name.to_string(), client);
    }
    Ok(clients.get(name).unwrap())
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
) {
    use futures_util::StreamExt;

    let mut assistant_message_id: Option<u64> = None;
    let mut assistant_seq: u64 = 0;
    let mut pending_reasoning: Vec<shuvarie_db::ReasoningSegment> = Vec::new();
    let mut reasoning_started: Option<std::time::Instant> = None;
    let mut pending_text = String::new();
    let mut tool_seq: u64 = 0;
    let mut turn_tool_records: Vec<crate::tool_record::ToolRecord> = Vec::new();
    let mut pending_tool_args: std::collections::HashMap<String, PendingTool> =
        std::collections::HashMap::new();
    let mut pending_worker_starts: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    let mut outcome = StreamOutcome::Finished;
    // Set when the manager stream reaches `Done`. Sending `Event::StreamDone`
    // is deferred until the merged stream is exhausted, so worker receiver
    // items queued behind it (a slow subagent's final tool results) are
    // delivered to the TUI before the turn is committed.
    let mut done: Option<(String, TokenUsage)> = None;

    while let Some(item) = stream.next().await {
        match item {
            shuvarie_llm::StreamItem::Delta { text } if !text.is_empty() => {
                pending_text.push_str(&text);
                {
                    let mut ts = turn_state.lock().await;
                    ts.pending_text = pending_text.clone();
                }
                let _ = event_tx.send(Event::TokenReceived { content: text }).await;
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
            }
            shuvarie_llm::StreamItem::Reasoning { .. } => {}
            shuvarie_llm::StreamItem::ToolStart {
                name,
                args,
                worker,
                call_id,
            } => {
                let args_json = args.to_string();
                pending_tool_args.insert(
                    call_id.clone(),
                    PendingTool {
                        args_json,
                        started: std::time::Instant::now(),
                    },
                );
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
                }
                let _ = event_tx
                    .send(Event::ToolStarted {
                        name,
                        args,
                        worker,
                        call_id,
                    })
                    .await;
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
                let (args_json, duration_ms) = match pending_tool_args.remove(&call_id) {
                    Some(pending) => (
                        pending.args_json,
                        pending.started.elapsed().as_millis() as u64,
                    ),
                    None => (String::new(), 0),
                };
                let worker_name = worker.as_deref();
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
                let text = if text.is_empty() && !pending_text.is_empty() {
                    std::mem::take(&mut pending_text)
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
                    &pending_text,
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
                    &pending_text,
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
                    &pending_text,
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
                // Run compaction: summarize the head of the stored session
                // so the next turn sends [summary, tail] instead of the full
                // history.
                let mut compacted = false;
                if let Some(sid) = session.lock().await.id
                    && let Ok(stored) = store.load_session(sid).await
                    && let Some(plan) =
                        crate::compaction::select_plan(&stored.messages, keep_recent_tokens)
                {
                    let head = &stored.messages[plan.start..plan.cut];
                    let records = span_tool_records(&stored, plan.start..plan.cut);
                    let head_text = crate::compaction::serialize_head(head, &records, plan.start);
                    let _ = event_tx.send(Event::CompactionStarted).await;
                    let summary = crate::compaction::summarize(&client, &model, &head_text).await;
                    let _ = event_tx.send(Event::CompactionFinished).await;
                    match summary {
                        Ok(summary) => {
                            if let Ok(msg) = store.append_summary(sid, &summary).await {
                                session.lock().await.summary_seq = Some(msg.seq);
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
                outcome = StreamOutcome::Overflowed { compacted };
                done = None;
                break;
            }
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
        guard.tool_records.append(&mut turn_tool_records);
        drop(guard);
        if let Some(id) = id {
            if let Some(msg_id) = assistant_message_id {
                let _ = store
                    .update_message(msg_id, &text, &reasoning, false, combined, cost)
                    .await;
                let _ = store.truncate_undo_log(id).await;
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
                    .append_assistant_message(id, &text, &reasoning, false, combined, cost)
                    .await
                {
                    Ok(msg) => {
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
/// dense message indices matching the loaded session's message order so the
/// transcript serializer can attach them to their messages.
fn span_tool_records(
    stored: &shuvarie_db::StoredSession,
    span: std::ops::Range<usize>,
) -> Vec<crate::tool_record::ToolRecord> {
    let id_to_index: std::collections::HashMap<u64, usize> = stored
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id, i))
        .collect();
    stored
        .tool_calls
        .iter()
        .filter(|tc| {
            id_to_index
                .get(&tc.message_id)
                .is_some_and(|&idx| span.contains(&idx))
        })
        .map(|tc| {
            let mut record = crate::tool_record::ToolRecord::from_stored(tc.clone());
            if let Some(idx) = id_to_index.get(&record.message_id) {
                record.message_seq = *idx as u64;
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
    let guard = session.lock().await;
    let Some(id) = guard.id else {
        return;
    };
    drop(guard);
    if let Ok(msg) = store
        .append_assistant_message(
            id,
            "",
            reasoning,
            false,
            shuvarie_llm::TokenUsage::default(),
            0.0,
        )
        .await
    {
        *assistant_message_id = Some(msg.id);
        *assistant_seq = msg.seq;
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

fn parse_file_change(json: &str) -> Option<FileChange> {
    if json.is_empty() {
        return None;
    }
    serde_json::from_str::<FileChange>(json).ok()
}

async fn persist_interrupted_turn(
    turn_state: Option<Arc<Mutex<TurnState>>>,
    store: &mut Store,
    session: &Option<Arc<Mutex<Session>>>,
    _event_tx: &Sender<Event>,
) {
    let (text, reasoning, msg_id, tool_records) = match turn_state {
        Some(ts_arc) => {
            let ts = ts_arc.lock().await;
            (
                ts.pending_text.clone(),
                ts.pending_reasoning.clone(),
                ts.assistant_message_id,
                ts.tool_records.clone(),
            )
        }
        None => return,
    };

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
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
            )
            .await;
        {
            let mut g = s.lock().await;
            g.push_assistant(text.clone());
            let seq = g.messages.len() - 1;
            g.reasoning.insert(seq as u64, reasoning);
            g.interrupted.insert(seq as u64, true);
            g.tool_records.extend(tool_records);
        }
    } else if !text.is_empty()
        && store
            .append_assistant_message(
                id,
                &text,
                &reasoning,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
            )
            .await
            .is_ok()
    {
        let mut g = s.lock().await;
        g.push_assistant(text);
        let seq = g.messages.len() - 1;
        g.interrupted.insert(seq as u64, true);
    }
}

fn build_client(pc: &ProviderConfig) -> Result<ProviderClient, String> {
    let kind = crate::catalog::provider_type(&pc.kind);
    let base_url = crate::catalog::base_url_for(&pc.kind, pc.base_url.as_deref());
    ProviderClient::build(kind, pc.api_key.as_deref(), base_url.as_deref())
        .map_err(|e| e.to_string())
}

async fn persist_stream_error(
    assistant_message_id: Option<u64>,
    pending_text: &str,
    pending_reasoning: &[shuvarie_db::ReasoningSegment],
    session: &Arc<Mutex<Session>>,
    store: &mut Store,
) {
    if pending_text.is_empty() && pending_reasoning.is_empty() {
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
                pending_text,
                pending_reasoning,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
            )
            .await;
    } else if !pending_text.is_empty() {
        let _ = store
            .append_assistant_message(
                id,
                pending_text,
                pending_reasoning,
                true,
                shuvarie_llm::TokenUsage::default(),
                0.0,
            )
            .await;
    }
    let mut g = session.lock().await;
    g.push_assistant(pending_text.to_string());
    let seq = g.messages.len() - 1;
    if !pending_reasoning.is_empty() {
        g.reasoning.insert(seq as u64, pending_reasoning.to_vec());
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
    use selune::ProviderType;
    use shuvarie_llm::StreamItem;
    use shuvarie_llm::TokenUsage;

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
}
