//! The decision service: the config's named decisions compiled into System One
//! requests, the shell and tool checks that consult them, and the connection
//! that answers.
//!
//! The service is deliberately *not* on the completion path. Decision models
//! are not generative — no streaming, no tools, no text — so they have their
//! own connection (`connections.kdl`'s `decision-providers` / `decision`) and
//! their own client.
//!
//! # Fail-safe degradation
//!
//! [`Decisions::build`] never fails. A problem that would stop a check from
//! running — an unknown decision name, a malformed rubric, a broken regex, no
//! active decision connection — is recorded, surfaced to the user as a startup
//! warning, and makes the affected check follow its own `on-error` policy
//! (default [`Verb::Ask`]). A configured check must never *silently* disappear:
//! that would quietly widen access beyond what the config asked for.
//!
//! # Off by default
//!
//! Decision models reach a network endpoint, so the feature is inert until the
//! config opts in ([`DecisionsConfig::is_enabled`], `enabled #true` in the
//! `decisions` section). While it is off nothing is compiled, no connection is
//! built, and every consumer reports the gate rather than silently doing
//! nothing.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;
use shuvarie_config::{
    Config, Connections, DecisionConfig, DecisionType, RankingConfig, ShellCheck, ShellCheckSource,
    ToolCheck, Verb,
};
use shuvarie_decision::{
    ChoiceOption, Decision, DecisionAnswer, DecisionApiType, DecisionClient, DecisionKind,
    DecisionOutcome, MAX_QUESTIONS, NoulCriteria, ScoreLevel,
};
use shuvarie_llm::{ProviderClient, StreamItem, TokenUsage, WorkerRequest};

use crate::permissions::{Decision as Verdict, ShellMatcher, collapse_whitespace};

/// The compiled decisions, checks, and connection.
pub struct Decisions {
    /// Whether the config enabled decision models. Everything here is inert
    /// when it is false.
    enabled: bool,
    /// Defined decisions by name, validated against the protocol.
    definitions: BTreeMap<String, Decision>,
    /// How a question's options are ranked (`ranking { decision … }`), from the
    /// config's global block; a scene may name its own entry on top of it.
    ranking: RankingConfig,
    /// The configured shell checks, in declaration order.
    checks: Vec<CompiledCheck>,
    /// The configured tool check (`permissions { tool-check { … } }`), when the
    /// config has one.
    tool_check: Option<CompiledToolCheck>,
    /// The active decision connection's client, when one is usable. Swappable
    /// while the program runs: the TUI registers decision providers mid-session,
    /// and a check that kept asking the connection from startup would report
    /// "no decision provider is selected" until a restart.
    client: RwLock<Option<DecisionClient>>,
    /// The completion connection a `worker` check runs on, refreshed per turn.
    /// `None` until a turn sets one, which leaves such a check following its own
    /// `on-error` policy rather than passing.
    worker: RwLock<Option<WorkerCheck>>,
    /// Everything that would stop a check from running, for a startup warning.
    problems: Vec<String>,
}

/// The completion connection a `worker` check runs on, plus the worker
/// preambles of the scene it runs under.
///
/// This is the one part of the service that is not fixed at startup. A decision
/// check asks its own System One connection about a `decisions` entry, both of
/// which the config settles once; a `worker` check runs on the *completion*
/// path instead — the model the session is using, and a worker defined in the
/// active scene's `subagents`. Both change as the session runs, so the turn
/// refreshes this rather than the service freezing it at startup.
struct WorkerCheck {
    client: ProviderClient,
    model: String,
    /// Worker name -> its system prompt, from the scene's `subagents`.
    workers: BTreeMap<String, String>,
}

impl std::fmt::Debug for Decisions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decisions")
            .field("enabled", &self.enabled)
            .field("definitions", &self.definitions.len())
            .field("checks", &self.checks.len())
            .field("tool_check", &self.tool_check.is_some())
            .field(
                "client",
                &self.client_clone().map(|client| client.label().to_string()),
            )
            .field("worker", &self.worker_clone().is_some())
            .field("problems", &self.problems)
            .finish()
    }
}

impl Decisions {
    /// Compile the config's decisions and shell checks against the configured
    /// connection. See the module docs: this never fails.
    pub fn build(config: &Config, connections: &Connections) -> Self {
        // Decision models reach a network endpoint, so nothing is asked until the
        // config opts in.
        if !config.decisions.is_enabled() {
            return Self::off(
                &config.permissions.checks,
                config.permissions.tool_check.as_ref(),
            );
        }
        let mut problems = Vec::new();
        let mut definitions = BTreeMap::new();
        for (name, definition) in &config.decisions.decisions {
            match compile_decision(name, definition) {
                Ok(decision) => {
                    definitions.insert(name.clone(), decision);
                }
                Err(error) => problems.push(error),
            }
        }
        let checks = config
            .permissions
            .checks
            .iter()
            .map(|check| CompiledCheck::build(check, &definitions))
            .collect::<Vec<_>>();
        for check in &checks {
            if let Some(problem) = &check.problem {
                problems.push(problem.clone());
            }
        }
        // A `worker` check names a `subagents` entry, and those live per scene:
        // the turn resolves the worker in the scene it runs under, so a check
        // whose name no scene defines could never resolve and would follow
        // `on-error` on every command. Reported here rather than per turn, where
        // it would be a startup warning repeated by every command.
        for check in &checks {
            let CheckSource::Worker(name) = &check.source else {
                continue;
            };
            if check.problem.is_some() {
                continue;
            }
            let defined = config.scenes.scenes.values().any(|scene| {
                !scene.subagents.disabled
                    && scene
                        .subagents
                        .workers
                        .get(name)
                        .is_some_and(|worker| !worker.disabled)
            });
            if !defined {
                problems.push(format!(
                    "shell check names worker `{name}`, which no scene's `subagents` defines"
                ));
                continue;
            }
            // The check runs a worker with no tools and one turn whatever the
            // worker is configured with — a check judges, it must not act — so a
            // `toolset` it will not honour is worth saying rather than silently
            // ignoring.
            let wants_tools = config.scenes.scenes.values().any(|scene| {
                !scene.subagents.disabled
                    && scene.subagents.workers.get(name).is_some_and(|worker| {
                        !worker.disabled
                            && worker.toolset != Some(shuvarie_config::SubagentToolset::None)
                    })
            });
            if wants_tools {
                problems.push(format!(
                    "shell check names worker `{name}`, whose `toolset` the check ignores (a check \
                     runs its worker with no tools)"
                ));
            }
        }
        let tool_check = config
            .permissions
            .tool_check
            .as_ref()
            .map(|check| CompiledToolCheck::build(check, &definitions));
        if let Some(problem) = tool_check.as_ref().and_then(|check| check.problem.as_ref()) {
            problems.push(problem.clone());
        }
        // Ranking is cosmetic, so an entry it cannot use is only warned about:
        // the user asked for it and would otherwise just see the options
        // unranked, with nothing saying why. Every scene that names its own
        // decision is checked too — a scene reaching for a decision that does
        // not exist would otherwise fail open in silence.
        if let Some(problem) = ranking_problem(&config.ranking, &definitions) {
            problems.push(problem);
        }
        for (scene, config) in &config.scenes.scenes {
            if let Some(problem) = ranking_problem(&config.ranking, &definitions) {
                problems.push(format!("scene `{scene}`: {problem}"));
            }
        }
        let client = match build_client(connections) {
            Ok(client) => Some(client),
            Err(error) => {
                // Only worth reporting when something would actually use it. A
                // `worker` check does not: it runs on the completion path, so a
                // config whose only checks are workers must not be told its
                // decision connection is missing.
                let needs_client = checks.iter().any(|check| check.source.is_decision())
                    || tool_check.is_some()
                    || !config.ranking.is_empty();
                if needs_client {
                    problems.push(error);
                }
                None
            }
        };
        Self {
            enabled: true,
            definitions,
            ranking: config.ranking.clone(),
            checks,
            tool_check,
            client: RwLock::new(client),
            worker: RwLock::new(None),
            problems,
        }
    }

    /// The service for a config that has not enabled decision models.
    ///
    /// No definition is compiled and no connection is built, but a *configured*
    /// check is still reported and still follows its own `on-error` policy: a
    /// check that cannot run must never pass for a check that passed, and its
    /// author should be told the section is off rather than see nothing at all.
    /// With no checks configured — the default — this is silently inert.
    fn off(checks: &[ShellCheck], tool_check: Option<&ToolCheck>) -> Self {
        let mut problems = Vec::new();
        let compiled = checks
            .iter()
            .map(|check| {
                // Compile the scope as usual so a narrowed check still covers
                // only the commands it names; only the answer is unavailable.
                let mut compiled = CompiledCheck::build(check, &BTreeMap::new());
                compiled.problem = Some(format!(
                    "shell check names {} `{}`, but decision models are disabled (add `enabled \
                     #true` to the `decisions` section)",
                    check.source.label(),
                    check.source.name()
                ));
                problems.push(compiled.problem.clone().expect("problem was just set"));
                compiled
            })
            .collect();
        let tool_check = tool_check.map(|check| {
            let mut compiled = CompiledToolCheck::build(check, &BTreeMap::new());
            compiled.problem = Some(format!(
                "tool check names decision `{}`, but decision models are disabled (add \
                 `enabled #true` to the `decisions` section)",
                check.decision
            ));
            problems.push(compiled.problem.clone().expect("problem was just set"));
            compiled
        });
        Self {
            enabled: false,
            definitions: BTreeMap::new(),
            ranking: RankingConfig::default(),
            checks: compiled,
            tool_check,
            client: RwLock::new(None),
            worker: RwLock::new(None),
            problems,
        }
    }

    /// Point the service at the connections' active decision provider after the
    /// TUI registers or edits one. Only the connection changes here — the
    /// decisions and checks themselves come from `config.kdl`, which is not
    /// reloaded mid-session.
    ///
    /// A selection that cannot be used clears the client rather than leaving the
    /// previous connection answering for one that was replaced, and its reason is
    /// returned so the caller can report it: like every other problem here, a
    /// connection that will not work must not fail silently.
    pub fn set_connection(&self, connections: &Connections) -> Result<(), String> {
        if !self.enabled {
            // Not an error the user caused — they registered a connection for a
            // feature they have not switched on. Saying so is the whole point of
            // reporting it here rather than failing silently.
            return Err(
                "decision models are disabled (add `enabled #true` to the `decisions` section)"
                    .to_string(),
            );
        }
        match build_client(connections) {
            Ok(client) => {
                *self.client_write() = Some(client);
                Ok(())
            }
            Err(error) => {
                *self.client_write() = None;
                Err(error)
            }
        }
    }

    /// The active connection's client, cloned out so no lock guard is held
    /// across the request.
    fn client_clone(&self) -> Option<DecisionClient> {
        self.client
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// A poisoned lock only means another thread panicked while swapping the
    /// connection; the client itself is never left half-written, so the guard
    /// is recovered rather than propagated.
    fn client_write(&self) -> std::sync::RwLockWriteGuard<'_, Option<DecisionClient>> {
        self.client
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Point the service's `worker` shell checks at this turn's completion
    /// connection and the scene's workers.
    ///
    /// Called once per turn, before the roster is built: the model a session
    /// runs on and the scene it runs under both change while the program runs,
    /// so a `worker` check that kept asking the connection from startup would
    /// be judging commands with a stale model — or one the session no longer
    /// has. A scene that defines no such worker simply leaves it absent from
    /// `workers`, and the check follows `on-error` rather than passing.
    pub fn set_worker_connection(
        &self,
        client: &ProviderClient,
        model: &str,
        scene: &crate::scenes::Scene,
    ) {
        let workers = worker_preambles(scene);
        *self.worker_write() = Some(WorkerCheck {
            client: client.clone(),
            model: model.to_string(),
            workers,
        });
    }

    /// The turn's completion connection, cloned out so no lock guard is held
    /// across the request.
    fn worker_clone(&self) -> Option<(ProviderClient, String, BTreeMap<String, String>)> {
        self.worker
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|worker| {
                (
                    worker.client.clone(),
                    worker.model.clone(),
                    worker.workers.clone(),
                )
            })
    }

    fn worker_write(&self) -> std::sync::RwLockWriteGuard<'_, Option<WorkerCheck>> {
        self.worker
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Everything that would stop a configured check from running, as
    /// user-facing messages for a startup warning. Empty when the service is
    /// fully usable.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// Whether the config enabled decision models. Consumers should consult this
    /// before offering a decision-backed feature at all.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether any shell check is configured.
    pub fn checks_configured(&self) -> bool {
        !self.checks.is_empty()
    }

    /// The defined decisions, for the `decide` tool and option ranking.
    pub fn definitions(&self) -> &BTreeMap<String, Decision> {
        &self.definitions
    }

    /// The active connection's client, when one is usable.
    pub fn client(&self) -> Option<DecisionClient> {
        self.client_clone()
    }

    /// Run the configured shell checks over `command` and tighten `verdict`.
    ///
    /// A command the rules already deny is returned untouched — there is
    /// nothing to learn about a command that will not run. Otherwise each
    /// matching check asks its decision and, when the answer reads as
    /// suspicious, turns an `allow` into an `ask`. `ask` and `deny` are never
    /// loosened: a decision model can only make the permission engine stricter,
    /// exactly like a scene's ask overlay.
    pub async fn check_shell(&self, command: &str, verdict: Verdict) -> Verdict {
        if matches!(verdict, Verdict::Deny { .. }) {
            return verdict;
        }
        let collapsed = collapse_whitespace(command);
        let mut verdict = verdict;
        for check in &self.checks {
            if !check.matches(command, &collapsed) {
                continue;
            }
            if matches!(verdict, Verdict::Deny { .. }) {
                break;
            }
            verdict = apply(verdict, check, self.ask(command, check).await);
        }
        verdict
    }

    /// Rank each prompt's options by how well they fit the question, leaving
    /// everything untouched on any failure — ranking is presentation, so it must
    /// never block or alter a question.
    pub async fn rank(
        &self,
        mut questions: Vec<crate::question::QuestionPrompt>,
        scene: Option<&RankingConfig>,
    ) -> Vec<crate::question::QuestionPrompt> {
        if !self.enabled {
            return questions;
        }
        // Every way out of here below is the same answer: the options the tool
        // asked for, in the order it asked them.
        let Some(name) = RankingConfig::resolve(&self.ranking, scene) else {
            return questions;
        };
        let Some(decision) = self.definitions.get(name) else {
            return questions;
        };
        let DecisionKind::Score { levels } = &decision.kind else {
            return questions;
        };
        let Some(client) = self.client_clone() else {
            return questions;
        };
        // One question per option, so the model judges each alternative on its
        // own.
        let mut planned: Vec<(usize, Vec<String>)> = Vec::new();
        let mut requests: Vec<Decision> = Vec::new();
        for (index, prompt) in questions.iter().enumerate() {
            let options = prompt.options.len();
            // Fewer than two options have nothing to reorder, and a prompt is
            // ranked whole or not at all: the request budget counts questions,
            // not prompts.
            if options < 2 || requests.len() + options > MAX_QUESTIONS {
                continue;
            }
            let names = (0..options)
                .map(|option| format!("option-{index}-{option}"))
                .collect::<Vec<_>>();
            for (name, option) in names.iter().zip(&prompt.options) {
                requests.push(Decision::score(
                    name.clone(),
                    option_instructions(&decision.instructions, option),
                    levels.iter().map(|level| level.description.clone()),
                ));
            }
            planned.push((index, names));
        }
        if planned.is_empty() {
            return questions;
        }
        let state = ranking_state(&questions, &planned);
        // A provider that refuses, times out, or answers nothing usable leaves
        // the question exactly as the tool asked it.
        let Ok(outcome) = client.evaluate(&state, &requests).await else {
            return questions;
        };
        for (index, names) in planned {
            let Some(scores) = option_scores(&outcome, &names) else {
                continue;
            };
            let prompt = &mut questions[index];
            let mut ranked = scores
                .into_iter()
                .zip(std::mem::take(&mut prompt.options))
                .collect::<Vec<_>>();
            // Stable, so options the model scored equally keep the order the
            // question offered them in.
            ranked.sort_by(|(a, _), (b, _)| b.total_cmp(a));
            prompt.options = ranked.into_iter().map(|(_, option)| option).collect();
        }
        questions
    }

    /// One check's answer — whether the command reads as suspicious, with the
    /// clause saying why — or the reason the answer could not be obtained.
    async fn ask(&self, command: &str, check: &CompiledCheck) -> Result<CheckAnswer, String> {
        if let Some(problem) = &check.problem {
            return Err(problem.clone());
        }
        match &check.source {
            CheckSource::Decision(name) => {
                let Some(client) = self.client_clone() else {
                    return Err("no decision provider is selected".to_string());
                };
                let Some(decision) = self.definitions.get(name) else {
                    return Err(format!("decision `{name}` is not defined"));
                };
                match client
                    .decide(&Value::String(command.to_string()), decision)
                    .await
                {
                    Ok(DecisionAnswer::Noul { probability }) => {
                        Ok(noul_answer(probability, check.threshold))
                    }
                    Ok(_) => Err(format!("decision `{name}` is not a `noul` decision")),
                    Err(error) => Err(error.to_string()),
                }
            }
            CheckSource::Worker(name) => self.ask_worker(command, name).await,
        }
    }

    /// Put a command to a `subagents` worker: one turn, no tools, an answer
    /// parsed strictly.
    ///
    /// A worker is a generative model, so its answer is prose rather than a
    /// probability and could say anything. It runs with no tools and one turn
    /// whatever the worker's configured `toolset` says — a check judges, it must
    /// not act — and an answer that is not the agreed `safe` / `suspicious` line
    /// is *unusable* rather than guessed at, leaving the verdict to `on-error`.
    async fn ask_worker(&self, command: &str, name: &str) -> Result<CheckAnswer, String> {
        let Some((client, model, workers)) = self.worker_clone() else {
            return Err("no completion connection is available for worker checks".to_string());
        };
        let Some(preamble) = workers.get(name) else {
            return Err(format!(
                "worker `{name}` is not defined in this scene's `subagents`"
            ));
        };
        let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel::<StreamItem>(16);
        let request = WorkerRequest {
            client: client.clone(),
            name: name.to_string(),
            // Not a roster spawn: this run is not activity the user watches, and
            // its items are drained below rather than surfaced.
            spawn: 0,
            model,
            preamble: preamble.clone(),
            task: worker_task(command),
            tools: Vec::new(),
            activity_tx,
            usage: std::sync::Arc::new(std::sync::Mutex::new(TokenUsage::default())),
            max_turns: 1,
            context_budget: None,
        };
        let reply = client.run_worker(&request).await?;
        // The answer is already in hand: close first so the drain is bounded by
        // what is buffered, and a sender kept alive inside a hook cannot hang
        // the check forever.
        activity_rx.close();
        while activity_rx.recv().await.is_some() {}
        parse_worker_verdict(&reply)
    }

    /// Run the configured tool check over a tool call and tighten `verdict`.
    ///
    /// A call the rules already deny is returned untouched — there is nothing
    /// to learn about a call that will not run. Otherwise the check asks its
    /// decision about the call and maps the returned label through the
    /// configured rules. `ask` and `deny` are never loosened: a decision model
    /// can only make the permission engine stricter, exactly like the shell
    /// checks.
    pub async fn check_tool(&self, tool: &str, detail: &str, verdict: Verdict) -> Verdict {
        if matches!(verdict, Verdict::Deny { .. }) {
            return verdict;
        }
        let Some(check) = &self.tool_check else {
            return verdict;
        };
        let outcome = self.ask_tool(tool, detail, check).await;
        apply_tool_choice(verdict, check, outcome)
    }

    /// The tool check's answer: the label the decision chose, or the reason it
    /// could not be obtained.
    async fn ask_tool(
        &self,
        tool: &str,
        detail: &str,
        check: &CompiledToolCheck,
    ) -> Result<String, String> {
        if let Some(problem) = &check.problem {
            return Err(problem.clone());
        }
        let Some(client) = self.client_clone() else {
            return Err("no decision provider is selected".to_string());
        };
        let Some(decision) = self.definitions.get(&check.decision) else {
            return Err(format!("decision `{}` is not defined", check.decision));
        };
        // The state names both halves of the call: which tool, and what it was
        // asked to do. A decision that only saw the name could not tell a
        // `read_file` of `/etc/passwd` from a harmless one.
        let state = tool_state(tool, detail);
        match client.decide(&state, decision).await {
            Ok(DecisionAnswer::Choice { choice, .. }) => Ok(choice),
            Ok(_) => Err(format!(
                "decision `{}` is not a `choice` decision",
                check.decision
            )),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// The state one ranking request judges: every prompt whose options are being
/// ranked, with its header and the labels the model is choosing between — a
/// per-option question can only judge fit against the whole set it belongs to.
fn ranking_state(
    questions: &[crate::question::QuestionPrompt],
    planned: &[(usize, Vec<String>)],
) -> Value {
    let prompts = planned
        .iter()
        .map(|(index, names)| {
            let prompt = &questions[*index];
            let options = names
                .iter()
                .zip(&prompt.options)
                .map(|(name, option)| (name.clone(), option.label.clone()))
                .collect::<BTreeMap<String, String>>();
            serde_json::json!({
                "question": prompt.question,
                "header": prompt.header,
                "options": options,
            })
        })
        .collect::<Vec<_>>();
    Value::Array(prompts)
}

/// One rubric score per option, in the order the options were asked about;
/// `None` when the provider left any of them out.
fn option_scores(outcome: &DecisionOutcome, names: &[String]) -> Option<Vec<f64>> {
    names
        .iter()
        .map(|name| outcome.answer(name).and_then(DecisionAnswer::score))
        .collect()
}

/// One option's ranking question: the configured decision's own instructions
/// plus a final sentence naming the alternative under judgement, so one
/// question per option can share a single request.
fn option_instructions(instructions: &str, option: &crate::question::QuestionOption) -> String {
    if option.description.trim().is_empty() {
        format!("{} Judge this option: \"{}\".", instructions, option.label)
    } else {
        format!(
            "{} Judge this option: \"{}\" — {}.",
            instructions, option.label, option.description
        )
    }
}

/// The scene's workers, by name, with the system prompt a `worker` check runs
/// each with: the worker's own configured `prelude`, or the built-in check
/// contract when it sets none.
///
/// A scene that switched its `subagents` off defines none, however its
/// per-worker entries read — and neither does a disabled worker. A built-in
/// worker's own prompt (which lives in the roster, not in config) is not used
/// here: a check asks its worker for a verdict, so it gets the verdict contract.
fn worker_preambles(scene: &crate::scenes::Scene) -> BTreeMap<String, String> {
    match scene.subagents() {
        Some(subagents) if !subagents.disabled => subagents
            .workers
            .iter()
            .filter(|(_, worker)| !worker.disabled)
            .map(|(name, worker)| {
                let preamble = worker
                    .system_prompts
                    .prelude
                    .clone()
                    .unwrap_or_else(|| DEFAULT_CHECK_PREAMBLE.to_string());
                (name.clone(), preamble)
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// A `noul` probability turned into an answer by the configured threshold.
///
/// The protocol returns a probability, not a verdict, and where the line falls
/// is the config's policy. Kept pure so the comparison stays testable without a
/// provider, exactly as the tighten-only fold it feeds does.
fn noul_answer(probability: f64, threshold: f64) -> CheckAnswer {
    if probability >= threshold {
        CheckAnswer::Suspicious(format!(
            "reads this command as suspicious ({probability:.2} ≥ {threshold:.2})"
        ))
    } else {
        CheckAnswer::Safe
    }
}

/// The system prompt a `worker` check runs with when the worker does not set
/// its own: the answer contract [`parse_worker_verdict`] enforces.
const DEFAULT_CHECK_PREAMBLE: &str = "\
You review a single shell command before an agentic coding assistant runs it. \
You will be given the command and nothing else.\n\n\
Judge whether it carries a destructive, exfiltrating, or privilege-escalating \
intent.\n\n\
Answer on one line, exactly one of:\n\
- `safe` — nothing about it warrants a human confirmation.\n\
- `suspicious: <reason>` — it does. Say why in one short sentence.\n\n\
Reply with that line only: no preamble, no explanation, no alternatives.";

/// How much of a worker's reason reaches the permission prompt: a verdict, not
/// an essay.
const MAX_WORKER_REASON_CHARS: usize = 200;

/// How much of an unrecognised answer is quoted back in the error.
const MAX_WORKER_ANSWER_CHARS: usize = 80;

/// What a `worker` check puts to its worker: the command, plus the answer
/// contract restated.
///
/// The preamble carries the contract too, but a worker the config gives its own
/// `prelude` replaces the default one — so the shape is asked for here as well,
/// where nothing can override it.
fn worker_task(command: &str) -> String {
    format!("Command:\n---\n{command}\n---\n\nAnswer with `safe` or `suspicious: <reason>`.")
}

/// The verdict a worker's answer carries, read strictly.
///
/// The agreed contract is one line: `safe`, or `suspicious` with an optional
/// `: reason`. Anything else — an empty answer, a verdict buried mid-paragraph,
/// an answer that argues both ways — is *unusable* rather than guessed at, and
/// the caller follows `on-error` instead. Reading intent out of prose would make
/// how strict the gate is depend on how the model felt that turn, which is
/// exactly what a permission gate must not do.
fn parse_worker_verdict(reply: &str) -> Result<CheckAnswer, String> {
    let line = reply
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "the worker answered with nothing".to_string())?;
    // A model often wraps its answer in markup even when told not to.
    let line = line
        .trim_matches(|c: char| matches!(c, '`' | '*' | '"' | '\'' | '#'))
        .trim();
    let lowered = line.to_ascii_lowercase();
    // The token has to stand alone: `safely` is not `safe`.
    let ends_word = |after: usize| {
        lowered.get(after..).is_some_and(|tail| {
            tail.is_empty()
                || matches!(tail.chars().next(), Some(':' | '-' | '—' | ',' | '.' | ' '))
        })
    };
    if lowered.starts_with("safe") && ends_word("safe".len()) {
        return Ok(CheckAnswer::Safe);
    }
    if lowered.starts_with("suspicious") && ends_word("suspicious".len()) {
        let reason = collapse_whitespace(
            line["suspicious".len()..]
                .trim_start_matches([':', '-', '—', ',', '.', ' '])
                .trim(),
        );
        return Ok(CheckAnswer::Suspicious(if reason.is_empty() {
            "reads this command as suspicious".to_string()
        } else {
            format!(
                "reads this command as suspicious: {}",
                truncate_chars(&reason, MAX_WORKER_REASON_CHARS)
            )
        }));
    }
    Err(format!(
        "the worker did not answer `safe` or `suspicious` (it said `{}`)",
        truncate_chars(line, MAX_WORKER_ANSWER_CHARS)
    ))
}

/// `text` capped to `max` characters, marked when it was cut.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

/// Fold one check's outcome into a verdict, tightening only.
///
/// Split out from the async plumbing so the policy — the part that decides how
/// much access a command gets — is testable without a provider.
fn apply(verdict: Verdict, check: &CompiledCheck, outcome: Result<CheckAnswer, String>) -> Verdict {
    let tighten = |verdict: Verdict, reason: String| match verdict {
        // Never loosen: an existing `ask`/`deny` keeps its own reason, which
        // names the rule that decided it.
        Verdict::Allow => Verdict::Ask { reason },
        other => other,
    };
    let label = check.source.label();
    let name = check.source.name();
    match outcome {
        // `why` reads as a clause of its own ("reads this command as suspicious
        // (0.83 ≥ 0.50)"), so the prefix only has to name who answered.
        Ok(CheckAnswer::Suspicious(why)) => tighten(verdict, format!("{label} `{name}` {why}")),
        Ok(CheckAnswer::Safe) => verdict,
        Err(error) => {
            let reason = format!("{label} `{name}` could not run: {error}");
            match check.on_error {
                Verb::Allow => verdict,
                Verb::Ask => tighten(verdict, reason),
                // A deny is already the strictest verdict, so one that exists
                // keeps the reason naming the rule that decided it.
                Verb::Deny => match verdict {
                    Verdict::Deny { .. } => verdict,
                    _ => Verdict::Deny { reason },
                },
            }
        }
    }
}

/// The state a tool check puts to its decision: which tool, and what this call
/// asked it to do.
fn tool_state(tool: &str, detail: &str) -> Value {
    serde_json::json!({ "tool": tool, "call": detail })
}

/// Fold one tool check's outcome into a verdict, tightening only.
///
/// Split out from the async plumbing so the policy — the part that decides how
/// much access a tool call gets — is the pure function `apply` is, and is
/// testable without a provider. The outcome is the label the decision chose, or
/// the reason it could not be obtained; a label the config does not map is an
/// unusable answer too, since it would otherwise never fire.
fn apply_tool_choice(
    verdict: Verdict,
    check: &CompiledToolCheck,
    outcome: Result<String, String>,
) -> Verdict {
    let tighten = |verdict: Verdict, verb: Verb, reason: String| match verdict {
        Verdict::Allow => match verb {
            Verb::Allow => Verdict::Allow,
            Verb::Ask => Verdict::Ask { reason },
            Verb::Deny => Verdict::Deny { reason },
        },
        // Never loosen: an existing `ask`/`deny` keeps its own reason, which
        // names the rule that decided it.
        other => other,
    };
    match outcome {
        Ok(label) => match check.rules.get(&label) {
            Some(verb) => tighten(
                verdict,
                *verb,
                format!("decision `{}` chose `{label}`", check.decision),
            ),
            None => tighten(
                verdict,
                check.on_error,
                format!(
                    "decision `{}` chose option `{label}`, which is not mapped",
                    check.decision
                ),
            ),
        },
        Err(error) => tighten(
            verdict,
            check.on_error,
            format!("decision `{}` could not run: {error}", check.decision),
        ),
    }
}

/// Which commands a check covers.
enum CheckScope {
    /// Every command (`check-all`).
    All,
    /// Commands matching any of these patterns.
    Patterns(Vec<ShellMatcher>),
}

/// One compiled `shell-patterns { check { … } }` rule.
struct CompiledCheck {
    /// Which model answers this check.
    source: CheckSource,
    scope: CheckScope,
    /// What an unusable answer means.
    on_error: Verb,
    /// The `noul` probability at or above which the command is suspicious. Read
    /// only by a [`CheckSource::Decision`] check, whose answer is a probability.
    threshold: f64,
    /// Why this check cannot evaluate, if it cannot.
    problem: Option<String>,
}

/// Which model answers a compiled check.
///
/// The two are different machinery and are kept apart here rather than resolved
/// to one shape: a decision is a System One question answered with a `noul`
/// probability over a connection the config fixes once, while a worker is a
/// one-turn completion answered in prose by a model the session supplies.
#[derive(Debug, Clone, PartialEq)]
enum CheckSource {
    /// A `decisions` entry: a `noul` question answered with a probability.
    Decision(String),
    /// A `subagents` worker: a one-turn completion answered in prose.
    Worker(String),
}

impl CheckSource {
    /// The name the check asked for.
    fn name(&self) -> &str {
        match self {
            Self::Decision(name) | Self::Worker(name) => name,
        }
    }

    /// Whether this asks a decision model rather than a worker.
    fn is_decision(&self) -> bool {
        matches!(self, Self::Decision(_))
    }

    /// What to call it when reporting a verdict to the user.
    fn label(&self) -> &'static str {
        match self {
            Self::Decision(_) => "decision",
            Self::Worker(_) => "worker",
        }
    }
}

/// What a check decided about a command.
#[derive(Debug, Clone, PartialEq)]
enum CheckAnswer {
    /// The command reads as safe: the verdict stands as the rules left it.
    Safe,
    /// The command reads as suspicious: the clause saying why.
    Suspicious(String),
}

impl CompiledCheck {
    fn build(check: &ShellCheck, definitions: &BTreeMap<String, Decision>) -> Self {
        let mut problem = None;
        let source = match &check.source {
            ShellCheckSource::Decision(name) => {
                // A decision check reads a probability, so only a `noul` decision
                // fits.
                match definitions.get(name) {
                    Some(Decision {
                        kind: DecisionKind::Noul { .. },
                        ..
                    }) => {}
                    Some(_) => {
                        problem = Some(format!(
                            "shell check names decision `{name}`, which is not a `noul` decision"
                        ));
                    }
                    None => {
                        problem = Some(format!(
                            "shell check names decision `{name}`, which is not defined"
                        ));
                    }
                }
                CheckSource::Decision(name.clone())
            }
            // A worker lives in a scene's `subagents`, and the scene a command
            // runs under is only known as the session runs, so whether it exists
            // is settled per turn ([`Decisions::set_worker_connection`]) — and
            // reported against every scene at build time.
            ShellCheckSource::Worker(name) => CheckSource::Worker(name.clone()),
        };
        // A scope that cannot be compiled would otherwise match nothing and
        // silently drop the check, so it degrades to covering every command
        // and lets `on-error` decide.
        let scope = if check.patterns.is_empty() {
            CheckScope::All
        } else {
            match check
                .patterns
                .iter()
                .map(|pattern| ShellMatcher::compile(pattern, check.kind))
                .collect::<Result<Vec<_>, String>>()
            {
                Ok(matchers) => CheckScope::Patterns(matchers),
                Err(error) => {
                    problem.get_or_insert(error);
                    CheckScope::All
                }
            }
        };
        Self {
            source,
            scope,
            on_error: check.on_error,
            threshold: check.threshold,
            problem,
        }
    }

    fn matches(&self, command: &str, collapsed: &str) -> bool {
        match &self.scope {
            CheckScope::All => true,
            CheckScope::Patterns(matchers) => matchers
                .iter()
                .any(|matcher| matcher.matches(command, collapsed)),
        }
    }
}

/// One compiled `tool-check { … }` rule.
struct CompiledToolCheck {
    /// The `decisions` entry this check asks.
    decision: String,
    /// Option label -> verb, for the labels the config maps.
    rules: BTreeMap<String, Verb>,
    /// What an unusable answer means.
    on_error: Verb,
    /// Why this check cannot evaluate, if it cannot.
    problem: Option<String>,
}

impl CompiledToolCheck {
    fn build(check: &ToolCheck, definitions: &BTreeMap<String, Decision>) -> Self {
        let mut rules = BTreeMap::new();
        for rule in &check.rules {
            rules.insert(rule.label.clone(), rule.verb);
        }
        let mut problem = None;
        // A tool check reads an option label, so only a `choice` decision fits.
        match definitions.get(&check.decision) {
            Some(Decision {
                kind: DecisionKind::Choice { options },
                ..
            }) => {
                // A label the decision does not declare can never come back from
                // the provider, so a typo would silently fall through `on-error`
                // on every call: report it instead.
                for label in rules.keys() {
                    if !options.iter().any(|option| &option.label == label) {
                        problem = Some(format!(
                            "tool check maps option `{label}`, which decision `{}` does not \
                             declare",
                            check.decision
                        ));
                        break;
                    }
                }
            }
            Some(_) => {
                problem = Some(format!(
                    "tool check names decision `{}`, which is not a `choice` decision",
                    check.decision
                ));
            }
            None => {
                problem = Some(format!(
                    "tool check names decision `{}`, which is not defined",
                    check.decision
                ));
            }
        }
        Self {
            decision: check.decision.clone(),
            rules,
            on_error: check.on_error,
            problem,
        }
    }
}

/// Why a `ranking` block cannot rank, if it cannot: the name it gives must be
/// defined and must be a `score` decision. `None` when the block is off or the
/// naming is usable.
fn ranking_problem(
    ranking: &RankingConfig,
    definitions: &BTreeMap<String, Decision>,
) -> Option<String> {
    if ranking.disabled {
        return None;
    }
    let name = ranking.decision.as_deref()?;
    match definitions.get(name) {
        Some(Decision {
            kind: DecisionKind::Score { .. },
            ..
        }) => None,
        Some(_) => Some(format!(
            "option ranking names decision `{name}`, which is not a `score` decision"
        )),
        None => Some(format!(
            "option ranking names decision `{name}`, which is not defined"
        )),
    }
}

/// Convert one config definition into its protocol form, validating it against
/// the protocol's bounds so a bad rubric is reported at startup rather than on
/// the first command.
fn compile_decision(name: &str, config: &DecisionConfig) -> Result<Decision, String> {
    let kind = match config.kind {
        DecisionType::Noul => DecisionKind::Noul {
            criteria: match (&config.yes, &config.no) {
                (Some(yes), Some(no)) => Some(NoulCriteria {
                    yes: yes.clone(),
                    no: no.clone(),
                }),
                _ => None,
            },
        },
        DecisionType::Choice => DecisionKind::Choice {
            options: config
                .options
                .iter()
                .map(|option| ChoiceOption {
                    label: option.label.clone(),
                    description: option.description.clone(),
                })
                .collect(),
        },
        DecisionType::Score => DecisionKind::Score {
            levels: config
                .levels
                .iter()
                .map(|level| ScoreLevel {
                    name: level.name.clone(),
                    description: level.description.clone(),
                })
                .collect(),
        },
    };
    let decision = Decision {
        name: name.to_string(),
        instructions: config.instructions.clone(),
        kind,
    };
    decision.validate().map_err(|error| error.to_string())?;
    Ok(decision)
}

/// The client for the active decision connection.
fn build_client(connections: &Connections) -> Result<DecisionClient, String> {
    let Some(active) = &connections.decision else {
        return Err(
            "shell checks are configured but no decision provider is selected in \
             `connections.kdl`"
                .to_string(),
        );
    };
    let Some(provider) = connections.decision_providers.get(&active.provider) else {
        return Err(format!(
            "the selected decision provider `{}` is not defined in `connections.kdl`",
            active.provider
        ));
    };
    let api_type = DecisionApiType::parse(&provider.kind).ok_or_else(|| {
        format!(
            "decision provider `{}` has unknown type `{}`",
            active.provider, provider.kind
        )
    })?;
    DecisionClient::build(
        api_type,
        active.provider.clone(),
        active.model.clone(),
        provider.base_url.clone().unwrap_or_default(),
        provider.api_key.as_deref(),
    )
    .map_err(|error| error.to_string())
}

/// The shell-check policy shared with the permission engine.
pub(crate) type SharedDecisions = Arc<Decisions>;

#[cfg(test)]
mod tests;
