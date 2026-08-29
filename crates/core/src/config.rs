use std::path::PathBuf;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use serde::{Deserialize, Serialize};

use crate::kdlfmt::{self, NodeNamed, finish, push_child, scalar, strings};
use crate::{CoreError, Result};

const CONFIG_DIR_NAME: &str = "shuvarie";
const CONFIG_FILE_NAME: &str = "config.kdl";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct Config {
    pub ui: UiPrefs,

    pub embedding: EmbeddingConfig,

    pub agent: AgentConfig,

    pub lsp: shuvarie_lsp::LspConfig,

    pub skills: SkillsConfig,

    pub context: ContextConfig,
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        max_turns: default_max_turns(),
        worker_max_turns: 0,
    }
}

fn default_max_turns() -> usize {
    0
}

/// Context-window management: bounds the input tokens sent to the LLM per model
/// call. Modeled on OpenCode's compaction/preserve approach but without an LLM
/// summarizer — older tool results are dropped (replaced with a short marker)
/// once the estimated request size exceeds the budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct ContextConfig {
    /// Enable the history-budget hook that trims old tool results per model
    /// call when the estimated request size exceeds the budget.
    pub enabled: bool,

    /// Tokens reserved for the model's reply and a safety buffer. The input
    /// budget is `context_length - reserved`.
    pub reserved: u64,

    /// Maximum chars of a tool result's text sent to the model. Larger outputs
    /// are truncated with a marker hinting the model to read ranges. `0`
    /// disables the cap.
    pub tool_output_max_chars: usize,

    /// Default context length used when the catalog has no entry for the
    /// active model.
    pub fallback_context_length: u64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reserved: default_reserved(),
            tool_output_max_chars: default_tool_output_max_chars(),
            fallback_context_length: default_fallback_context(),
        }
    }
}

fn default_reserved() -> u64 {
    20_000
}

fn default_tool_output_max_chars() -> usize {
    16_000
}

fn default_fallback_context() -> u64 {
    128_000
}

impl ContextConfig {
    /// Tokens kept verbatim as the "tail" during per-call history trimming.
    /// 25% of the usable budget, clamped to [2_000, 15_000] tokens (estimated
    /// at ~4 chars/token).
    pub fn preserve_recent_tokens(&self, context_length: u64) -> u64 {
        let usable = context_length.saturating_sub(self.reserved);
        let pct = usable / 4;
        pct.clamp(2_000, 15_000)
    }

    /// Usable input-token budget for the given model context length.
    pub fn usable(&self, context_length: u64) -> u64 {
        context_length.saturating_sub(self.reserved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct SkillsConfig {
    pub enabled: bool,
    pub dirs: Vec<String>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct EmbeddingConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: None,
            model: None,
            dimensions: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct UiPrefs {
    /// Target frames per second for the TUI render loop. `0` disables the cap
    /// (one draw per event, the original behavior). Defaults to 60.
    pub frame_rate: u32,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            frame_rate: default_frame_rate(),
        }
    }
}

fn default_frame_rate() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct AgentConfig {
    pub max_turns: usize,

    pub worker_max_turns: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        default_agent_config()
    }
}

impl AgentConfig {
    pub fn effective_max_turns(&self) -> usize {
        effective(self.max_turns)
    }

    pub fn effective_worker_max_turns(&self) -> usize {
        effective(self.worker_max_turns)
    }
}

fn effective(value: usize) -> usize {
    if value == 0 { usize::MAX } else { value }
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir().ok_or_else(|| {
        CoreError::ConfigIo(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory for this platform",
        ))
    })?;
    Ok(dir.join(CONFIG_DIR_NAME))
}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        Ok(config_dir()?.join(CONFIG_FILE_NAME))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(Self::from_kdl(&contents)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CoreError::ConfigIo(e)),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_kdl())?;
        Ok(())
    }

    fn from_kdl(contents: &str) -> Result<Self> {
        let doc = kdlfmt::parse(contents)?;
        let mut config = Self::default();

        for node in doc.nodes() {
            let view = node.view();
            match node.name().value() {
                "ui" => {
                    config.ui.frame_rate =
                        view.integer("frame-rate", config.ui.frame_rate as i128) as u32;
                }
                "embedding" => {
                    config.embedding.enabled = view.boolean("enabled", config.embedding.enabled);
                    config.embedding.provider = view.string("provider");
                    config.embedding.model = view.string("model");
                    config.embedding.dimensions = if view.child_is_present("dimensions") {
                        Some(view.integer(
                            "dimensions",
                            i128::from(config.embedding.dimensions.unwrap_or(0)),
                        ) as u32)
                    } else {
                        config.embedding.dimensions
                    };
                }
                "agent" => {
                    config.agent.max_turns =
                        view.integer("max-turns", config.agent.max_turns as i128) as usize;
                    config.agent.worker_max_turns = view
                        .integer("worker-max-turns", config.agent.worker_max_turns as i128)
                        as usize;
                }
                "lsp" => {
                    config.lsp.enabled = view.boolean("enabled", config.lsp.enabled);
                    for server in node.children().map(|c| c.nodes()).unwrap_or_default() {
                        if server.name().value() != "server" {
                            continue;
                        }
                        let Some(name) = server.entries().first().and_then(|e| match e.value() {
                            KdlValue::String(s) => Some(s.to_string()),
                            KdlValue::Integer(i) => Some(i.to_string()),
                            _ => None,
                        }) else {
                            continue;
                        };
                        let server_view = server.view();
                        let spec = shuvarie_lsp::LspServerSpec {
                            command: server_view.strings("command").unwrap_or_default(),
                            extensions: server_view.strings("extensions").unwrap_or_default(),
                            auto_start: server_view.boolean("auto-start", true),
                            root_markers: server_view.strings("root-markers").unwrap_or_default(),
                        };
                        config.lsp.servers.insert(name, spec);
                    }
                }
                "skills" => {
                    config.skills.enabled = view.boolean("enabled", config.skills.enabled);
                    config.skills.dirs = view.strings("dirs").unwrap_or_default();
                }
                "context" => {
                    config.context.enabled = view.boolean("enabled", config.context.enabled);
                    config.context.reserved =
                        view.integer("reserved", config.context.reserved as i128) as u64;
                    config.context.tool_output_max_chars = view.integer(
                        "tool-output-max-chars",
                        config.context.tool_output_max_chars as i128,
                    ) as usize;
                    config.context.fallback_context_length = view.integer(
                        "fallback-context-length",
                        config.context.fallback_context_length as i128,
                    ) as u64;
                }
                _ => {}
            }
        }

        Ok(config)
    }

    fn to_kdl(&self) -> String {
        let mut doc = KdlDocument::new();

        let mut ui = KdlNode::new("ui");
        push_child(
            &mut ui,
            scalar("frame-rate", i128::from(self.ui.frame_rate)),
        );
        doc.nodes_mut().push(ui);

        let mut embedding = KdlNode::new("embedding");
        push_child(&mut embedding, scalar("enabled", self.embedding.enabled));
        if let Some(provider) = &self.embedding.provider {
            push_child(&mut embedding, scalar("provider", provider.as_str()));
        }
        if let Some(model) = &self.embedding.model {
            push_child(&mut embedding, scalar("model", model.as_str()));
        }
        if let Some(dimensions) = self.embedding.dimensions {
            push_child(&mut embedding, scalar("dimensions", i128::from(dimensions)));
        }
        doc.nodes_mut().push(embedding);

        let mut agent = KdlNode::new("agent");
        push_child(
            &mut agent,
            scalar("max-turns", self.agent.max_turns as i128),
        );
        push_child(
            &mut agent,
            scalar("worker-max-turns", self.agent.worker_max_turns as i128),
        );
        doc.nodes_mut().push(agent);

        let mut lsp = KdlNode::new("lsp");
        push_child(&mut lsp, scalar("enabled", self.lsp.enabled));
        for (name, spec) in &self.lsp.servers {
            let mut server = KdlNode::new("server");
            server.entries_mut().push(KdlEntry::new(name.as_str()));
            if !spec.command.is_empty() {
                push_child(&mut server, strings("command", &spec.command).unwrap());
            }
            if !spec.extensions.is_empty() {
                push_child(
                    &mut server,
                    strings("extensions", &spec.extensions).unwrap(),
                );
            }
            push_child(&mut server, scalar("auto-start", spec.auto_start));
            if !spec.root_markers.is_empty() {
                push_child(
                    &mut server,
                    strings("root-markers", &spec.root_markers).unwrap(),
                );
            }
            push_child(&mut lsp, server);
        }
        doc.nodes_mut().push(lsp);

        let mut skills = KdlNode::new("skills");
        push_child(&mut skills, scalar("enabled", self.skills.enabled));
        if let Some(dirs) = strings("dirs", &self.skills.dirs) {
            push_child(&mut skills, dirs);
        }
        doc.nodes_mut().push(skills);

        let mut context = KdlNode::new("context");
        push_child(&mut context, scalar("enabled", self.context.enabled));
        push_child(
            &mut context,
            scalar("reserved", self.context.reserved as i128),
        );
        push_child(
            &mut context,
            scalar(
                "tool-output-max-chars",
                self.context.tool_output_max_chars as i128,
            ),
        );
        push_child(
            &mut context,
            scalar(
                "fallback-context-length",
                self.context.fallback_context_length as i128,
            ),
        );
        doc.nodes_mut().push(context);

        finish(doc)
    }
}
