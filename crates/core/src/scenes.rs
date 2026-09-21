//! Scene runtime: resolving a session's active scene, gating its tool
//! roster, and injecting its prompts around the outgoing request.
//!
//! A scene is a named bundle from the `scenes` config section (plus the
//! `scene.d` drop-ins): a system-prompt prelude/interlude, injected wrapper
//! prompts, and tool availability. The built-in default scene is code, not
//! config: it reproduces the unconfigured behavior except for its hard-coded
//! default interlude ([`DEFAULT_INTERLUDE`]), which makes it re-enterable in
//! the middle of a session. It is the fallback when nothing else resolves.

use std::collections::BTreeMap;

use shuvarie_config::{
    Hooks, SceneConfig, SceneToolVerb, SceneToolsConfig, ScenesConfig, SubagentConfig,
    SubagentsConfig, ToolOverride,
};
use shuvarie_llm::{ChatMsg, Role};

/// Display name of the built-in default scene (never serialized to config).
pub const DEFAULT_SCENE_NAME: &str = "Default";

/// The built-in default scene's picker description.
pub const DEFAULT_SCENE_DESCRIPTION: &str = "Built-in behavior, no scene configured";

/// The built-in default scene's interlude, injected at the top of the outgoing
/// history: it tells the model the earlier turns may have run under a scene
/// with its own instructions or tool restrictions, and to keep going under the
/// default behavior. Hedged because a session may also live its whole life
/// under the built-in scene — the interlude rides every request either way,
/// just like a configured scene's.
pub const DEFAULT_INTERLUDE: &str = "The earlier turns of this session may have run under a different scene, \
possibly with its own instructions or tool restrictions. Treat any scene-specific \
constraints in the history as belonging to those earlier turns, and continue \
under the default behavior.";

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

    /// The scene's interlude: injected at the top of the outgoing history on
    /// every request after a mid-session switch, so the model knows earlier
    /// turns ran under a different scene. The built-in default scene carries
    /// the hard-coded [`DEFAULT_INTERLUDE`].
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
/// The built-in default scene injects only its interlude; a configured scene
/// without one changes nothing.
///
/// Hooks fire around every prior user prompt / assistant reply. The turn
/// markers wrap each turn: a turn starts with its user prompt (so
/// `before-each.turn` composes before it) and ends with its assistant reply
/// (so `after-each.turn` composes after the reply hook).
pub fn inject_history(
    scene: &Scene,
    prior: &[ChatMsg],
    pending_user: Option<&str>,
) -> Vec<ChatMsg> {
    let before = scene.before_each();
    let after = scene.after_each();

    let mut out = Vec::with_capacity(prior.len() * 2 + 4);
    if let Some(interlude) = scene.interlude() {
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
        // `disabled: Some(false)` survive. The config `disabled` switch has
        // no "off" form, so these overrides are built programmatically.
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
                            disabled
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
    }

    #[test]
    fn injection_passes_default_history_through() {
        let prior = vec![ChatMsg::user("hi"), ChatMsg::assistant("hello")];
        let injected = inject_history(&Scene::default(), &prior, Some("next"));
        assert_eq!(
            injected,
            vec![
                ChatMsg::system(DEFAULT_INTERLUDE),
                ChatMsg::user("hi"),
                ChatMsg::assistant("hello"),
            ],
            "the built-in scene injects only its default interlude"
        );
        let empty = inject_history(&Scene::default(), &[], Some("next"));
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
        let injected = inject_history(&scene, &prior, Some("u3"));
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
    }
}
