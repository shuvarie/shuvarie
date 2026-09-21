//! Resolved MCP server specs.
//!
//! These mirror `shuvarie_config::McpConfig` in runtime form: placeholders
//! (`$VAR` / `${VAR}` in env values and HTTP header values) are resolved by
//! the conversion in `shuvarie-core` before a spec is built, so the manager
//! never sees unresolved text.

use std::collections::BTreeMap;

/// The environment for a spawned MCP stdio server process: `inherit` selects
/// whether the parent's environment is passed through, and `entries` adds
/// overrides and additions on top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSpec {
    pub inherit: bool,
    pub entries: BTreeMap<String, String>,
}

impl Default for EnvSpec {
    /// The semantic default mirrors `EnvsConfig`: inherit the parent
    /// environment, with no extra entries. (`bool`'s derived default would
    /// be `false`, the opposite of what users expect.)
    fn default() -> Self {
        Self {
            inherit: true,
            entries: BTreeMap::new(),
        }
    }
}

/// One configured MCP server: the transport is chosen by the variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerSpec {
    /// Spawned as a child process and talked to over stdio.
    Stdio {
        command: String,
        args: Vec<String>,
        envs: EnvSpec,
    },
    /// Streamable HTTP transport (`http(s)://…`).
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl McpServerSpec {
    /// The transport label used in status lines and errors.
    pub fn transport_label(&self) -> &'static str {
        match self {
            Self::Stdio { .. } => "stdio",
            Self::Http { .. } => "http",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_labels() {
        let stdio = McpServerSpec::Stdio {
            command: "npx".into(),
            args: vec!["-y".into()],
            envs: EnvSpec::default(),
        };
        assert_eq!(stdio.transport_label(), "stdio");
        let http = McpServerSpec::Http {
            url: "https://example/mcp".into(),
            headers: BTreeMap::new(),
        };
        assert_eq!(http.transport_label(), "http");
    }

    #[test]
    fn env_spec_defaults_to_inheriting_no_entries() {
        let envs = EnvSpec::default();
        assert!(envs.inherit);
        assert!(envs.entries.is_empty());
    }
}
