//! Scene runtime: resolving a session's active scene, gating its tool
//! roster, and injecting its prompts around the outgoing request.
//!
//! A scene is a named bundle from the `scenes` config section (plus the
//! `scene.d` drop-ins): a system-prompt prelude/interlude, injected wrapper
//! prompts, and tool availability. Code-defined builtin scenes ([
//! `builtin_set`]) form the bottom layer under the configured ones: a
//! configured scene of the same name merges field-wise over its builtin
//! (overriding only the fields it sets), and any other name joins on top.
//! The built-in default scene stays code, not config: it reproduces the
//! unconfigured behavior except for its hard-coded default interlude
//! ([`DEFAULT_INTERLUDE`]), which makes it re-enterable in the middle of a
//! session. It is the fallback when nothing else resolves.

use std::collections::BTreeMap;

use shuvarie_config::{
    Hooks, SceneConfig, SceneToolVerb, SceneToolsConfig, ScenesConfig, SubagentConfig,
    SubagentToolset, SubagentsConfig, SystemPromptsConfig, ToolOverride,
};
use shuvarie_llm::{ChatMsg, Role};

/// Display name of the built-in default scene (never serialized to config).
pub const DEFAULT_SCENE_NAME: &str = "Default";

/// The built-in default scene's picker description.
pub const DEFAULT_SCENE_DESCRIPTION: &str = "Built-in behavior, no scene configured";

/// The built-in default scene's interlude, injected at the top of the outgoing
/// history of the first request after the built-in scene is entered
/// mid-session: it tells the model the earlier turns may have run under a
/// scene with its own instructions or tool restrictions, and to keep going
/// under the default behavior.
pub const DEFAULT_INTERLUDE: &str = "The earlier turns of this session may have run under a different scene, \
possibly with its own instructions or tool restrictions. Treat any scene-specific \
constraints in the history as belonging to those earlier turns, and continue \
under the default behavior.";

/// The builtin Advisor scene's name and picker description.
pub const ADVISOR_SCENE_NAME: &str = "Advisor";

/// The builtin Advisor scene's picker description.
pub const ADVISOR_SCENE_DESCRIPTION: &str =
    "Advice and planning: inspection and research only, no modifications";

/// The builtin Reviewer scene's name and picker description.
pub const REVIEWER_SCENE_NAME: &str = "Reviewer";

/// The builtin Reviewer scene's picker description.
pub const REVIEWER_SCENE_DESCRIPTION: &str =
    "Code review: read the code, run checks, report findings without applying them";

/// The builtin Orchestrator scene's name and picker description.
pub const ORCHESTRATOR_SCENE_NAME: &str = "Orchestrator";

/// The builtin Orchestrator scene's picker description.
pub const ORCHESTRATOR_SCENE_DESCRIPTION: &str =
    "Agent orchestration: decompose the task, delegate to subagent workers, verify, integrate";

/// The inspection tool roster kept in the read-only builtin scenes. It is
/// the `read` worker toolset plus the scene-independent bookkeeping tools
/// (`question`, `todo`); write, edit, and command tools stay disabled.
const READ_ROSTER: [&str; 10] = [
    "read_file",
    "list_dir",
    "grep",
    "glob",
    "skill",
    "lsp",
    "webfetch",
    "web_search",
    "question",
    "todo",
];

pub(crate) const ADVISOR_INTERLUDE: &str = "The earlier turns of this session may have run under a different scene, \
possibly with its own instructions, tool restrictions, or edits in flight. Treat \
any scene-specific constraints in the history as belonging to those earlier turns. \
From here on you are in Advisor mode: analyze and advise with your inspection \
tools only; do not modify the workspace while in this scene.";

pub(crate) const REVIEWER_INTERLUDE: &str = "The earlier turns of this session may have run under a different scene, \
possibly with its own instructions or tool restrictions. Treat any scene-specific \
constraints in the history as belonging to those earlier turns. From here on you \
are in Reviewer mode: establish and verify the state of the code and report \
findings; running checks and commands is fine, but this scene does not modify files.";

pub(crate) const ORCHESTRATOR_INTERLUDE: &str = "The earlier turns of this session may have run under a different scene, \
possibly with its own instructions, tool restrictions, or worker runs. Treat any \
scene-specific constraints in the history as belonging to those earlier turns. \
From here on you are in Orchestrator mode: drive the task by delegating to your \
subagent workers — brief them precisely, then verify and integrate their results.";

/// The Advisor's prelude: replaces the built-in agent preamble.
pub(crate) const ADVISOR_PRELUDE: &str = "You are Shuvarie in Advisor mode, an agentic coding assistant answering \
advisory questions: analysis, planning, and judgment rather than execution. \
Research freely with your inspection tools, but file-writing, editing, and \
command running stay out of scope in this scene. \
Deliver judgment, not edits: an assessment of the situation, the options on the \
table with their trade-offs, a clear recommendation, and the next steps. \
When a decision belongs to the user, ask with the `question` tool instead of \
assuming. Reference concrete files and line numbers to ground your advice.";

/// The Reviewer's prelude: replaces the built-in agent preamble.
pub(crate) const REVIEWER_PRELUDE: &str = "You are Shuvarie in Reviewer mode, an agentic coding assistant reviewing \
code rather than changing it. Establish the facts first: find the relevant \
changes (for example `git diff` and `git log` through `run_shell`), read the \
surrounding code for context, and verify every claim against the actual \
implementation; run build, test, and lint commands as evidence when useful. \
You may run commands and read anything, but you do not modify files — report \
findings instead of fixing them. \
Structure the review: findings ordered by severity (blocker, major, minor, nit), \
each with file paths, line references, the evidence, and a concrete suggested \
fix; finish with an overall verdict and what deserves manual attention.";

/// The Orchestrator's prelude: replaces the built-in agent preamble.
pub(crate) const ORCHESTRATOR_PRELUDE: &str = "You are Shuvarie in Orchestrator mode, an agent orchestrator: you \
drive the task by delegating work to your subagent workers and integrating \
what they report back. \
First frame the task: restate it concretely, with its goal, the constraints, \
and the workspace context that matters; when it is ambiguous, ask the user \
with the `question` tool before delegating. \
Then decompose: split the task into subtasks a single worker can complete in \
one shot, and delegate each to the best-suited worker with a self-contained \
brief — the goal, the exact context it needs, the constraints, and how \
success will be checked. Batch independent briefs in one message so the \
spawns run in parallel. \
The workers are single-shot and do not remember earlier runs: keep every \
brief complete, and when a follow-up is needed, spawn it with the exact \
excerpts it must respond to rather than referring back. \
Verify results as they land — check the workspace or re-read the reports \
before trusting them; the workers run with restricted toolsets and no \
memory of you. \
Integration is yours: glue code, fix-ups, and final assembly are done with \
your own tools, not delegated away. \
Report the outcome: what was delegated to whom and why, what each run \
returned, the evidence that it is correct, and what — if anything — remains.";

/// A builtin Orchestrator planning-bench member's prelude: the members are
/// single-shot subagent workers, so each prelude says the member reports once.
pub(crate) const ARCHITECT_PRELUDE: &str = "You are the architect member of a planning bench convened by an \
orchestrator agent. You receive one task: the problem under planning plus \
your mandate. Inspect the workspace as far as it helps, then shape the \
approach: how the problem divides, the key decisions with their trade-offs, \
and the order of work. Stay concise and self-contained: you give one report \
and get no follow-up.";

/// A builtin Orchestrator planning-bench member's prelude.
pub(crate) const CRITIC_PRELUDE: &str = "You are the critic member of a planning bench convened by an \
orchestrator agent. You receive one task: the plan or approach under review \
plus your mandate. Inspect the workspace as far as it helps, then attack it: \
hidden assumptions, failure modes, missing requirements, and cheaper or \
simpler alternatives — fair and concrete, every concern marked with its \
likelihood and impact. Stay concise and self-contained: you give one report \
and get no follow-up.";

/// A builtin Orchestrator planning-bench member's prelude.
pub(crate) const SCOUT_PRELUDE: &str = "You are the scout member of a planning bench convened by an \
orchestrator agent. You receive one task: what to find out plus your mandate. \
Inspect the workspace and report the facts: the layout that matters, where \
the relevant pieces live, their current state, and anything the orchestrator \
should know before planning. Stay concise and self-contained: you give one \
report and get no follow-up.";

/// A code-defined builtin scene: its fixed name plus config. Builtin scenes
/// never serialize to config; they materialize under the configured scenes
/// through [`builtin_set`].
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinScene {
    pub name: &'static str,
    pub config: SceneConfig,
}

impl BuiltinScene {
    const fn new(name: &'static str, config: SceneConfig) -> Self {
        Self { name, config }
    }
}

/// The code-defined builtin scenes, in picker order under the built-in
/// default scene: the plain default behavior, then the advisory, review, and
/// orchestration modes.
pub fn builtin_scenes() -> Vec<BuiltinScene> {
    vec![
        BuiltinScene::new(ADVISOR_SCENE_NAME, advisor_config()),
        BuiltinScene::new(REVIEWER_SCENE_NAME, reviewer_config()),
        BuiltinScene::new(ORCHESTRATOR_SCENE_NAME, orchestrator_config()),
    ]
}

/// The builtin scenes as the bottom config layer: [`ScenesConfig::stack`]
/// merges configured scenes of the same name field-wise over them (so a user
/// redefinition overrides only what it sets) and adds every other scene on
/// top. No builtin is named "Default": the built-in default scene stays
/// identity-less and a configured scene of that name remains a distinct row.
pub fn builtin_set() -> ScenesConfig {
    ScenesConfig {
        default: None,
        scenes: builtin_scenes()
            .into_iter()
            .map(|builtin| (builtin.name.to_string(), builtin.config))
            .collect(),
    }
}

/// The Advisor builtin scene: read-only inspection plus the advisory
/// protocol. Re-enterable mid-session via its interlude.
fn advisor_config() -> SceneConfig {
    SceneConfig {
        description: Some(ADVISOR_SCENE_DESCRIPTION.into()),
        subagents: SubagentsConfig {
            disabled: false,
            workers: BTreeMap::from([
                ("run_tests".into(), disabled_worker()),
                ("edit_files".into(), disabled_worker()),
            ]),
        },
        system_prompts: SystemPromptsConfig {
            prelude: Some(ADVISOR_PRELUDE.into()),
            interlude: Some(ADVISOR_INTERLUDE.into()),
            ..Default::default()
        },
        tools: read_only_tools(&[]),
        ..Default::default()
    }
}

/// The Reviewer builtin scene: inspection plus commands, no modifications.
fn reviewer_config() -> SceneConfig {
    SceneConfig {
        description: Some(REVIEWER_SCENE_DESCRIPTION.into()),
        subagents: SubagentsConfig {
            disabled: false,
            workers: BTreeMap::from([("edit_files".into(), disabled_worker())]),
        },
        system_prompts: SystemPromptsConfig {
            prelude: Some(REVIEWER_PRELUDE.into()),
            interlude: Some(REVIEWER_INTERLUDE.into()),
            ..Default::default()
        },
        tools: read_only_tools(&["run_shell"]),
        ..Default::default()
    }
}

/// The Orchestrator builtin scene: the orchestration protocol, a planning
/// bench of three contrasting members, and parallel delegation
/// (`tool-concurrency 4`). The orchestrator keeps the full tool roster, so
/// integration, fix-ups, and verification also run through its own tools.
fn orchestrator_config() -> SceneConfig {
    SceneConfig {
        description: Some(ORCHESTRATOR_SCENE_DESCRIPTION.into()),
        tool_concurrency: Some(4),
        subagents: SubagentsConfig {
            disabled: false,
            workers: BTreeMap::from([
                (
                    "architect".into(),
                    bench_worker(
                        "Planning bench member: shape the approach for the task — how it \
                          divides, the key decisions and their trade-offs, and the order \
                          of work.",
                        ARCHITECT_PRELUDE,
                    ),
                ),
                (
                    "critic".into(),
                    bench_worker(
                        "Planning bench member: stress-test the plan or approach — \
                          challenge assumptions, surface failure modes and missing \
                          requirements, and gather counter-evidence from the workspace.",
                        CRITIC_PRELUDE,
                    ),
                ),
                (
                    "scout".into(),
                    bench_worker(
                        "Planning bench member: recon the workspace for the planning \
                          inputs — layout, where the relevant pieces live, and their \
                          current state.",
                        SCOUT_PRELUDE,
                    ),
                ),
            ]),
        },
        system_prompts: SystemPromptsConfig {
            prelude: Some(ORCHESTRATOR_PRELUDE.into()),
            interlude: Some(ORCHESTRATOR_INTERLUDE.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A read-only tool roster: `disable-all` keeps only the explicitly
/// re-enabled tools — the [`READ_ROSTER`] plus `extra` (the Reviewer adds
/// `run_shell`).
fn read_only_tools(plus: &[&str]) -> SceneToolsConfig {
    let tools = READ_ROSTER
        .iter()
        .chain(plus)
        .map(|tool| (tool.to_string(), enabled_tool()))
        .collect();
    SceneToolsConfig {
        verb: Some(SceneToolVerb::DisableAll),
        tools,
    }
}

fn enabled_tool() -> ToolOverride {
    ToolOverride {
        disabled: Some(false),
        ask: None,
    }
}

fn disabled_worker() -> SubagentConfig {
    SubagentConfig {
        disabled: true,
        ..Default::default()
    }
}

fn bench_worker(description: &str, prelude: &str) -> SubagentConfig {
    SubagentConfig {
        description: Some(description.into()),
        toolset: Some(SubagentToolset::Read),
        system_prompts: SystemPromptsConfig {
            prelude: Some(prelude.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// One row of the scene switcher: the switch identity handed back to
/// `Command::SwitchScene` (`None` = the built-in default scene) plus display
/// data. Identity, not name — a configured scene named "Default" stays
/// distinct from (and switchable next to) the built-in one. `switchable`
/// marks scenes that may be entered mid-session (they carry an interlude);
/// the rest can only start a session.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneListEntry {
    pub id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub switchable: bool,
}

/// Whether the scene may be entered in the middle of a session: it has a
/// non-empty interlude, the injected prompt that tells the model the scene
/// changed. Scenes without one can only start a session. Only configured
/// scenes are asked — the built-in default scene carries the hard-coded
/// [`DEFAULT_INTERLUDE`] and is always switchable.
pub fn is_switchable(config: &SceneConfig) -> bool {
    config
        .system_prompts
        .interlude
        .as_deref()
        .is_some_and(|interlude| !interlude.trim().is_empty())
}

/// The scene a request runs under: a configured scene or the built-in
/// default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scene {
    /// The configured scene; `None` = the built-in default scene.
    config: Option<SceneConfig>,
    /// The configured name; `None` = the built-in default scene.
    name: Option<String>,
}

impl Scene {
    /// Resolves the scene a session runs under: the persisted name while it
    /// still resolves, else the built-in default (a scene deleted from
    /// config since the session used it falls back).
    pub fn resolve(scenes: &ScenesConfig, stored: Option<&str>) -> Self {
        let config = stored.and_then(|name| scenes.scene(name)).cloned();
        Self {
            name: config
                .as_ref()
                .map(|_| stored.expect("resolvable scene has a name").to_string()),
            config,
        }
    }

    pub fn is_default(&self) -> bool {
        self.config.is_none()
    }

    /// The scene's name, or [`DEFAULT_SCENE_NAME`] for the built-in scene.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(DEFAULT_SCENE_NAME)
    }

    pub fn description(&self) -> Option<&str> {
        self.config.as_ref().and_then(|c| c.description.as_deref())
    }

    /// The scene's prelude: it replaces the built-in agent preamble.
    pub fn prelude(&self) -> Option<&str> {
        self.config
            .as_ref()
            .and_then(|c| c.system_prompts.prelude.as_deref())
    }

    /// The scene's interlude: injected at the top of the outgoing history of
    /// the first request after a mid-session switch, so the model knows
    /// earlier turns ran under a different scene. The built-in default scene
    /// carries the hard-coded [`DEFAULT_INTERLUDE`].
    pub fn interlude(&self) -> Option<&str> {
        match &self.config {
            Some(c) => c.system_prompts.interlude.as_deref(),
            None => Some(DEFAULT_INTERLUDE),
        }
    }

    pub fn before_each(&self) -> Option<&Hooks> {
        self.config
            .as_ref()
            .and_then(|c| c.system_prompts.before_each.as_ref())
    }

    pub fn after_each(&self) -> Option<&Hooks> {
        self.config
            .as_ref()
            .and_then(|c| c.system_prompts.after_each.as_ref())
    }

    /// The scene's tool availability rules.
    pub fn tools(&self) -> ToolScene {
        ToolScene::build(self.config.as_ref().map(|c| &c.tools))
    }

    /// The scene's tool-concurrency setting: how many tool calls the agent
    /// may run concurrently within one assistant message (a batch of worker
    /// spawns, or an otherwise parallel-capable tool). The unset default is
    /// sequential (`1`); a configured `0` clamps to it.
    pub fn tool_concurrency(&self) -> usize {
        self.config
            .as_ref()
            .and_then(|c| c.tool_concurrency)
            .unwrap_or(1)
            .max(1)
    }

    /// The scene's subagent overrides (roster kill switch + per-worker
    /// entries).
    pub fn subagents(&self) -> Option<&SubagentsConfig> {
        self.config.as_ref().map(|c| &c.subagents)
    }

    /// A named worker's override entry.
    pub fn worker(&self, name: &str) -> Option<&SubagentConfig> {
        self.config
            .as_ref()
            .and_then(|c| c.subagents.workers.get(name))
    }
}

/// The resolved tool availability of one scene (or a subagent override):
/// whether a tool is built into the roster at all, and whether it must ask
/// before running.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolScene {
    verb: Option<SceneToolVerb>,
    overrides: BTreeMap<String, ToolOverride>,
}

impl ToolScene {
    pub fn build(config: Option<&SceneToolsConfig>) -> Self {
        match config {
            Some(cfg) => Self {
                verb: cfg.verb,
                overrides: cfg.tools.clone(),
            },
            None => Self::default(),
        }
    }

    /// Whether the tool is built into the roster at all: under `disable-all`
    /// only explicitly re-enabled tools survive, otherwise per-tool
    /// `disabled` removes them.
    pub fn allows(&self, tool: &str) -> bool {
        let disabled = self.overrides.get(tool).and_then(|o| o.disabled);
        match self.verb {
            Some(SceneToolVerb::DisableAll) => disabled == Some(false),
            _ => disabled != Some(true),
        }
    }

    /// Whether the scene forces an ask verdict for the tool: `ask-all` or a
    /// per-tool `ask #true`. Disabled tools never ask — they simply do not
    /// exist for the turn.
    pub fn forces_ask(&self, tool: &str) -> bool {
        self.allows(tool)
            && (self.verb == Some(SceneToolVerb::AskAll)
                || self
                    .overrides
                    .get(tool)
                    .and_then(|o| o.ask)
                    .unwrap_or(false))
    }
}

/// Wraps the outgoing history with the scene's injected prompts. The pending
/// user prompt itself stays the plain `prompt` argument of the stream call,
/// so its `before-each` hooks are appended as trailing system messages.
///
/// The interlude is a one-shot transition marker: it is injected at the top
/// only when `announce_scene` is set — the first request after a mid-session
/// switch, the same request that persists the switch. On later requests (and
/// for a session that started under the scene) the history is left unwrapped
/// by it; the `before-each` / `after-each` hooks still wrap every message.
///
/// Hooks fire around every prior user prompt / assistant reply. The turn
/// markers wrap each turn: a turn starts with its user prompt (so
/// `before-each.turn` composes before it) and ends with its assistant reply
/// (so `after-each.turn` composes after the reply hook).
pub fn inject_history(
    scene: &Scene,
    prior: &[ChatMsg],
    pending_user: Option<&str>,
    announce_scene: bool,
) -> Vec<ChatMsg> {
    let before = scene.before_each();
    let after = scene.after_each();

    let mut out = Vec::with_capacity(prior.len() * 2 + 4);
    if announce_scene && let Some(interlude) = scene.interlude() {
        out.push(ChatMsg::system(interlude));
    }
    for msg in prior {
        match msg.role {
            Role::User => {
                if let Some(hooks) = before {
                    if let Some(turn) = &hooks.turn {
                        out.push(ChatMsg::system(turn));
                    }
                    if let Some(user) = &hooks.user_prompt {
                        out.push(ChatMsg::system(user));
                    }
                }
                out.push(msg.clone());
                if let Some(hooks) = after
                    && let Some(user) = &hooks.user_prompt
                {
                    out.push(ChatMsg::system(user));
                }
            }
            Role::Assistant => {
                if let Some(hooks) = before
                    && let Some(assistant) = &hooks.assistant_prompt
                {
                    out.push(ChatMsg::system(assistant));
                }
                out.push(msg.clone());
                if let Some(hooks) = after {
                    if let Some(assistant) = &hooks.assistant_prompt {
                        out.push(ChatMsg::system(assistant));
                    }
                    if let Some(turn) = &hooks.turn {
                        out.push(ChatMsg::system(turn));
                    }
                }
            }
            Role::System => out.push(msg.clone()),
        }
    }
    if pending_user.is_some()
        && let Some(hooks) = before
    {
        if let Some(turn) = &hooks.turn {
            out.push(ChatMsg::system(turn));
        }
        if let Some(user) = &hooks.user_prompt {
            out.push(ChatMsg::system(user));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_config::ScenesConfig;

    fn scene_config(text: &str) -> ScenesConfig {
        ScenesConfig::from_kdl(text).unwrap()
    }

    fn plan_scene(text: &str) -> Scene {
        let config = scene_config(text);
        Scene::resolve(&config, Some("Plan"))
    }

    #[test]
    fn resolves_the_persisted_scene() {
        let config = ScenesConfig::default();
        let resolved = Scene::resolve(&config, None);
        assert!(resolved.is_default());
        assert_eq!(resolved.display_name(), DEFAULT_SCENE_NAME);
        assert_eq!(resolved.description(), None);
        assert_eq!(resolved.prelude(), None);
        assert_eq!(resolved.interlude(), Some(DEFAULT_INTERLUDE));
        assert!(resolved.tools().allows("run_shell"));
        assert!(!resolved.tools().forces_ask("run_shell"));
    }

    #[test]
    fn switchability_needs_a_non_empty_interlude() {
        let config = scene_config(
            r#"
            scenes {
                scene name="Plan" {
                    system-prompts {
                        interlude "We switched to Plan mode."
                    }
                }
                scene name="Draft" {
                }
            }
        "#,
        );
        assert!(
            shuvarie_config::ScenesConfig::scene(&config, "Plan")
                .map(crate::scenes::is_switchable)
                .unwrap_or(false)
        );
        assert_eq!(
            shuvarie_config::ScenesConfig::scene(&config, "Draft")
                .map(crate::scenes::is_switchable),
            Some(false)
        );
        let blank = shuvarie_config::SceneConfig {
            system_prompts: shuvarie_config::SystemPromptsConfig {
                interlude: Some("   ".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            !crate::scenes::is_switchable(&blank),
            "a whitespace-only interlude is absent"
        );
    }

    #[test]
    fn unknown_scene_falls_back_to_builtin() {
        let config = ScenesConfig::default();
        let resolved = Scene::resolve(&config, Some("Ghost"));
        assert!(resolved.is_default(), "unresolvable name falls back");
        assert_eq!(resolved.display_name(), DEFAULT_SCENE_NAME);
    }

    #[test]
    fn a_configured_default_resolves_over_the_builtin() {
        let config = scene_config(
            r#"
            scenes {
                scene name="Default" {
                    description "custom default"
                    tools {
                        ask-all
                    }
                }
            }
        "#,
        );
        let resolved = Scene::resolve(&config, Some("Default"));
        assert!(!resolved.is_default(), "the configured scene wins");
        assert_eq!(resolved.display_name(), "Default");
        assert_eq!(resolved.description(), Some("custom default"));
        assert!(resolved.tools().forces_ask("run_shell"));
    }

    #[test]
    fn resolution_reads_the_configured_scene() {
        let resolved = plan_scene(
            r#"
            scenes {
                scene name="Plan" {
                    description "plan first"
                    system-prompts {
                        prelude "You are a planner."
                        interlude "We switched to Plan mode."
                    }
                }
            }
        "#,
        );
        assert!(!resolved.is_default());
        assert_eq!(resolved.display_name(), "Plan");
        assert_eq!(resolved.description(), Some("plan first"));
        assert_eq!(resolved.prelude(), Some("You are a planner."));
        assert_eq!(resolved.interlude(), Some("We switched to Plan mode."));
    }

    #[test]
    fn tool_scene_verbs_and_overrides() {
        // Under `disable-all` only tools explicitly re-enabled with
        // `disabled: Some(false)` survive; `enabled #true` in config now
        // spells that override.
        let tools_cfg = SceneToolsConfig {
            verb: Some(SceneToolVerb::DisableAll),
            tools: BTreeMap::from([
                (
                    "read_file".to_string(),
                    ToolOverride {
                        disabled: Some(false),
                        ask: None,
                    },
                ),
                (
                    "grep".to_string(),
                    ToolOverride {
                        disabled: Some(false),
                        ask: None,
                    },
                ),
                (
                    "run_shell".to_string(),
                    ToolOverride {
                        disabled: Some(false),
                        ask: Some(true),
                    },
                ),
            ]),
        };
        let tools = ToolScene::build(Some(&tools_cfg));
        assert!(!tools.allows("write_file"), "disable-all removes tools");
        assert!(tools.allows("read_file"), "explicit re-enable survives");
        assert!(tools.allows("grep"));
        assert!(tools.forces_ask("run_shell"), "re-enabled and asking");
        assert!(!tools.forces_ask("read_file"));

        let scene = plan_scene(
            r#"
            scenes {
                scene name="Plan" {
                    tools {
                        ask-all
                        tool "read_file" {
                            disabled #true
                        }
                    }
                }
            }
        "#,
        );
        let tools = scene.tools();
        assert!(
            !tools.allows("read_file"),
            "per-tool disabled beats ask-all"
        );
        assert!(tools.forces_ask("write_file"), "ask-all asks everything");
        assert!(tools.allows("write_file"));

        let scene = plan_scene(
            r#"
            scenes {
                scene name="Plan" {
                    tools {
                        tool "run_shell" {
                            ask #true
                        }
                    }
                }
            }
        "#,
        );
        let tools = scene.tools();
        assert!(tools.allows("run_shell"));
        assert!(tools.forces_ask("run_shell"));
        assert!(tools.allows("write_file"));
        assert!(!tools.forces_ask("write_file"));

        // `enabled #true` re-enables a tool under `disable-all`, straight
        // from config.
        let scene = plan_scene(
            r#"
            scenes {
                scene name="Plan" {
                    tools {
                        disable-all
                        tool "read_file" {
                            enabled #true
                        }
                        tool "run_shell" {
                            enabled #true
                            ask #true
                        }
                    }
                }
            }
        "#,
        );
        let tools = scene.tools();
        assert!(tools.allows("read_file"), "config re-enable survives");
        assert!(tools.allows("run_shell"));
        assert!(tools.forces_ask("run_shell"));
        assert!(!tools.allows("write_file"));
    }

    #[test]
    fn injection_passes_default_history_through() {
        let prior = vec![ChatMsg::user("hi"), ChatMsg::assistant("hello")];
        let injected = inject_history(&Scene::default(), &prior, Some("next"), false);
        assert_eq!(
            injected,
            vec![ChatMsg::user("hi"), ChatMsg::assistant("hello")],
            "without the announce nothing is injected, not even the built-in interlude"
        );
        let announced = inject_history(&Scene::default(), &prior, Some("next"), true);
        assert_eq!(
            announced,
            vec![
                ChatMsg::system(DEFAULT_INTERLUDE),
                ChatMsg::user("hi"),
                ChatMsg::assistant("hello"),
            ],
            "the built-in scene injects only its default interlude, on the announce"
        );
        let empty = inject_history(&Scene::default(), &[], Some("next"), true);
        assert_eq!(empty, vec![ChatMsg::system(DEFAULT_INTERLUDE)]);
    }

    #[test]
    fn injection_wraps_messages_and_appends_the_pending_hooks() {
        let scene = plan_scene(
            r#"
            scenes {
                scene name="Plan" {
                    system-prompts {
                        prelude "p"
                        interlude "switched"
                        before-each {
                            user-prompt "BU"
                            assistant-prompt "BA"
                            turn "BT"
                        }
                        after-each {
                            user-prompt "AU"
                            assistant-prompt "AA"
                            turn "AT"
                        }
                    }
                }
            }
        "#,
        );
        let prior = vec![
            ChatMsg::user("u1"),
            ChatMsg::assistant("a1"),
            ChatMsg::user("u2"),
        ];
        let injected = inject_history(&scene, &prior, Some("u3"), true);
        let rendered: Vec<(char, &str)> = injected
            .iter()
            .map(|m| {
                (
                    match m.role {
                        Role::System => 's',
                        Role::User => 'u',
                        Role::Assistant => 'a',
                    },
                    m.content.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                ('s', "switched"),
                ('s', "BT"),
                ('s', "BU"),
                ('u', "u1"),
                ('s', "AU"),
                ('s', "BA"),
                ('a', "a1"),
                ('s', "AA"),
                ('s', "AT"),
                ('s', "BT"),
                ('s', "BU"),
                ('u', "u2"),
                ('s', "AU"),
                ('s', "BT"),
                ('s', "BU"),
            ]
        );

        // The interlude is a one-shot announce; the hooks still wrap every
        // request.
        let later = inject_history(&scene, &prior, Some("u3"), false);
        assert!(
            later.iter().all(|m| m.content != "switched"),
            "the interlude rides only the announce request"
        );
        assert_eq!(
            later.first().map(|m| m.content.as_str()),
            Some("BT"),
            "hooks still wrap the first turn on later requests"
        );
    }

    // ---- the builtin scene registry ----------------------------------

    fn resolve_builtin(name: &str, user: Option<&str>) -> Scene {
        let mut set = builtin_set();
        if let Some(user_kdl) = user {
            let user_set = scene_config(&format!(
                "scenes {{ scene name=\"{name}\" {{\n{user_kdl}\n}} }}"
            ));
            set.stack(user_set);
        }
        Scene::resolve(&set, Some(name))
    }

    #[test]
    fn builtin_set_lists_the_three_modes_never_the_default() {
        let set = builtin_set();
        assert_eq!(set.default, None, "the builtin layer sets no default scene");
        let names: Vec<&str> = set.scenes.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            vec![
                ADVISOR_SCENE_NAME,
                ORCHESTRATOR_SCENE_NAME,
                REVIEWER_SCENE_NAME
            ],
            "builtins sit in the map, sorted by name; \"Default\" stays identity-less"
        );
        for (name, expected_description) in [
            (ADVISOR_SCENE_NAME, ADVISOR_SCENE_DESCRIPTION),
            (REVIEWER_SCENE_NAME, REVIEWER_SCENE_DESCRIPTION),
            (ORCHESTRATOR_SCENE_NAME, ORCHESTRATOR_SCENE_DESCRIPTION),
        ] {
            let config = set.scene(name).unwrap_or_else(|| panic!("{name} missing"));
            assert!(is_switchable(config), "{name} is re-enterable mid-session");
            let resolved = Scene::resolve(&set, Some(name));
            assert_eq!(resolved.description(), Some(expected_description));
            assert_eq!(resolved.display_name(), name);
        }
    }

    #[test]
    fn advisor_scene_gates_to_the_read_roster() {
        let scene = resolve_builtin(ADVISOR_SCENE_NAME, None);
        let tools = scene.tools();
        for tool in [
            "read_file",
            "list_dir",
            "grep",
            "glob",
            "lsp",
            "webfetch",
            "question",
            "todo",
            "skill",
        ] {
            assert!(tools.allows(tool), "Advisor keeps {tool}");
        }
        for tool in [
            "run_shell",
            "write_file",
            "edit_file",
            "apply_patch",
            "delete_file",
        ] {
            assert!(!tools.allows(tool), "Advisor drops {tool}");
            assert!(!tools.forces_ask(tool));
        }
        assert_eq!(scene.prelude().map(str::len), Some(ADVISOR_PRELUDE.len()));
        assert_eq!(scene.interlude(), Some(ADVISOR_INTERLUDE));
        // The write-capable workers cannot be spawned; the explorer stays.
        let roster = scene
            .subagents()
            .expect("advisor defines subagents")
            .workers
            .clone();
        assert!(roster.get("run_tests").unwrap().disabled);
        assert!(roster.get("edit_files").unwrap().disabled);
        assert!(!roster.contains_key("explore_workspace"));
    }

    #[test]
    fn reviewer_scene_allows_shell_but_no_edits() {
        let scene = resolve_builtin(REVIEWER_SCENE_NAME, None);
        let tools = scene.tools();
        for tool in ["read_file", "grep", "glob", "run_shell"] {
            assert!(tools.allows(tool), "Reviewer keeps {tool}");
        }
        for tool in ["write_file", "edit_file", "apply_patch", "delete_file"] {
            assert!(!tools.allows(tool), "Reviewer drops {tool}");
        }
        let roster = scene
            .subagents()
            .expect("reviewer defines subagents")
            .workers
            .clone();
        assert!(roster.get("edit_files").unwrap().disabled);
        assert!(!roster.contains_key("run_tests"), "checks still run");
    }

    #[test]
    fn orchestrator_scene_delegates_to_the_roster() {
        let scene = resolve_builtin(ORCHESTRATOR_SCENE_NAME, None);
        assert_eq!(
            scene.tool_concurrency(),
            4,
            "independent briefs spawn in one parallel batch"
        );
        let roster = scene
            .subagents()
            .expect("orchestrator defines subagents")
            .workers
            .clone();
        for member in ["architect", "critic", "scout"] {
            let bench = roster
                .get(member)
                .unwrap_or_else(|| panic!("{member} missing"));
            assert_eq!(bench.toolset, Some(SubagentToolset::Read));
            assert!(
                bench.description.is_some(),
                "{member} tells the orchestrator when to spawn it"
            );
            assert!(
                bench.system_prompts.prelude.is_some(),
                "{member} carries its mandate"
            );
            assert!(!bench.disabled);
        }
        // The built-in workers stay enabled: execution still routes through them.
        assert!(!roster.contains_key("explore_workspace"));
        assert!(!roster.contains_key("edit_files"));
    }

    #[test]
    fn tool_concurrency_resolves_to_sequential_by_default() {
        let resolved = Scene::resolve(&ScenesConfig::default(), None);
        assert_eq!(resolved.tool_concurrency(), 1);
    }

    #[test]
    fn a_zero_tool_concurrency_clamps_to_one() {
        let config = scene_config(
            r#"
            scenes {
                scene name="Solo" {
                    tool-concurrency 1
                }
            }
        "#,
        );
        assert_eq!(Scene::resolve(&config, Some("Solo")).tool_concurrency(), 1);
        // A hand-built `Some(0)` (parse clamps at the KDL boundary) still
        // clamps in the accessor.
        let mut scenes = ScenesConfig::default();
        scenes.scenes.insert(
            "Zero".into(),
            shuvarie_config::SceneConfig {
                tool_concurrency: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(Scene::resolve(&scenes, Some("Zero")).tool_concurrency(), 1);
    }

    #[test]
    fn a_user_scene_merges_over_its_builtin_fieldwise() {
        let scene = resolve_builtin(
            ADVISOR_SCENE_NAME,
            Some("description \"my advisor\"\ntools { ask-all }"),
        );
        assert_eq!(scene.description(), Some("my advisor"), "the override wins");
        assert_eq!(
            scene.prelude(),
            Some(ADVISOR_PRELUDE),
            "the unset fields inherit the builtin"
        );
        assert_eq!(scene.interlude(), Some(ADVISOR_INTERLUDE));
        // The tools section merges too: the builtin's `disable-all` verb is
        // replaced by `ask-all`, but the builtin's per-tool re-enables stay —
        // the read roster still runs only after asking (it explicitly allowed
        // under ask-all), while tools without an explicit allow also fall
        // back to plain allow + ask.
        let tools = scene.tools();
        assert!(tools.allows("read_file") && tools.forces_ask("read_file"));
        assert!(tools.forces_ask("run_shell"));
        assert!(tools.allows("write_file"));
    }
}
