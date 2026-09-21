//! The MCP manager's runtime config: converts the config-file types
//! ([`shuvarie_config::McpConfig`]) into [`shuvarie_mcp::McpServerSpec`]s,
//! resolving `$VAR` / `${VAR}` placeholders in env values and HTTP header
//! values, mirroring how `lsp_manager.rs` bridges the LSP config.

use std::collections::BTreeMap;

use shuvarie_mcp::{EnvSpec, McpServerSpec, SharedMcpManager};

/// Builds the runtime specs from the `tools { mcp { … } }` config section.
/// Env entries and HTTP header values carry `$VAR` / `${VAR}` placeholders,
/// resolved here against the environment (unknown vars stay as written).
pub fn mcp_specs(config: &shuvarie_config::McpConfig) -> BTreeMap<String, McpServerSpec> {
    let mut specs = BTreeMap::new();
    for (name, server) in &config.stdio {
        specs.insert(
            name.clone(),
            McpServerSpec::Stdio {
                command: server.command.clone(),
                args: server.args.clone(),
                envs: env_spec(&server.envs),
            },
        );
    }
    for (name, server) in &config.http {
        specs.insert(
            name.clone(),
            McpServerSpec::Http {
                url: server.url.clone(),
                headers: server
                    .headers
                    .iter()
                    .map(|(name, value)| (name.clone(), crate::catalog::resolve_env(value)))
                    .collect(),
            },
        );
    }
    specs
}

fn env_spec(envs: &shuvarie_config::EnvsConfig) -> EnvSpec {
    EnvSpec {
        inherit: envs.inherit,
        entries: envs
            .entries
            .iter()
            .map(|(name, value)| (name.clone(), crate::catalog::resolve_env(value)))
            .collect(),
    }
}

/// The shared handle core keeps around; re-exported here alongside the
/// conversion, mirroring [`crate::lsp_manager::SharedManager`].
pub type SharedMcp = SharedMcpManager;

/// A manager from the config section (specs env-resolved), for callers that
/// do not build one themselves.
pub fn mcp_manager(config: &shuvarie_config::McpConfig) -> SharedMcpManager {
    std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_mcp::McpManager::new(
        mcp_specs(config),
    )))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use shuvarie_config::{EnvsConfig, McpConfig, McpHttpConfig, McpStdioConfig};
    use shuvarie_mcp::McpServerSpec;

    use super::mcp_specs;

    #[test]
    fn converts_stdio_and_http_with_resolved_placeholders() {
        let config = McpConfig {
            stdio: BTreeMap::from([(
                "github".to_string(),
                McpStdioConfig {
                    command: "npx".to_string(),
                    args: vec![
                        "-y".to_string(),
                        "@modelcontextprotocol/server-github".to_string(),
                    ],
                    envs: EnvsConfig {
                        inherit: false,
                        entries: BTreeMap::from([
                            ("PLAIN".to_string(), "value".to_string()),
                            (
                                "UNSET".to_string(),
                                "$SHUVARIE_DEFINITELY_UNSET_12345".to_string(),
                            ),
                        ]),
                    },
                },
            )]),
            http: BTreeMap::from([(
                "deepwiki".to_string(),
                McpHttpConfig {
                    url: "https://mcp.deepwiki.com/mcp".to_string(),
                    headers: BTreeMap::from([(
                        "Authorization".to_string(),
                        "$SHUVARIE_DEFINITELY_UNSET_67890".to_string(),
                    )]),
                },
            )]),
        };

        let specs = mcp_specs(&config);
        assert_eq!(specs.len(), 2);
        let McpServerSpec::Stdio {
            command,
            args,
            envs,
        } = specs.get("github").expect("github")
        else {
            panic!("expected the stdio transport");
        };
        assert_eq!(command, "npx");
        assert_eq!(
            args,
            &vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-github".to_string()
            ]
        );
        assert!(!envs.inherit);
        // A plain value passes through; an unknown `$VAR` stays as written.
        assert_eq!(envs.entries.get("PLAIN").map(String::as_str), Some("value"));
        assert_eq!(
            envs.entries.get("UNSET").map(String::as_str),
            Some("$SHUVARIE_DEFINITELY_UNSET_12345")
        );

        let McpServerSpec::Http { url, headers } = specs.get("deepwiki").expect("deepwiki") else {
            panic!("expected the http transport");
        };
        assert_eq!(url, "https://mcp.deepwiki.com/mcp");
        assert_eq!(
            headers.get("Authorization").map(String::as_str),
            Some("$SHUVARIE_DEFINITELY_UNSET_67890")
        );
    }

    #[test]
    fn empty_config_yields_no_specs() {
        let config = McpConfig {
            stdio: BTreeMap::new(),
            http: BTreeMap::new(),
        };
        assert!(mcp_specs(&config).is_empty());
    }
}
