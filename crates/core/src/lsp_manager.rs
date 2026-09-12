use std::sync::Arc;

use tokio::sync::Mutex;

pub type SharedManager = Arc<Mutex<shuvarie_lsp::LspManager>>;

/// Builds the runtime config from its config-file mirror
/// ([`shuvarie_config::LspConfigRepr`]).
pub fn lsp_config_from_repr(repr: &shuvarie_config::LspConfigRepr) -> shuvarie_lsp::LspConfig {
    shuvarie_lsp::LspConfig {
        enabled: !repr.disabled,
        servers: repr
            .servers
            .iter()
            .map(|(name, spec)| (name.clone(), lsp_server_spec_from_repr(spec)))
            .collect(),
    }
}

/// Builds the config-file mirror ([`shuvarie_config::LspConfigRepr`]) of the
/// runtime config.
pub fn lsp_config_to_repr(config: &shuvarie_lsp::LspConfig) -> shuvarie_config::LspConfigRepr {
    shuvarie_config::LspConfigRepr {
        disabled: !config.enabled,
        servers: config
            .servers
            .iter()
            .map(|(name, spec)| (name.clone(), lsp_server_spec_to_repr(spec)))
            .collect(),
    }
}

fn lsp_server_spec_from_repr(
    spec: &shuvarie_config::LspServerSpecRepr,
) -> shuvarie_lsp::LspServerSpec {
    shuvarie_lsp::LspServerSpec {
        command: spec.command.clone(),
        extensions: spec.extensions.clone(),
        auto_start: !spec.no_auto_start,
        root_markers: spec.root_markers.clone(),
    }
}

fn lsp_server_spec_to_repr(
    spec: &shuvarie_lsp::LspServerSpec,
) -> shuvarie_config::LspServerSpecRepr {
    shuvarie_config::LspServerSpecRepr {
        command: spec.command.clone(),
        extensions: spec.extensions.clone(),
        no_auto_start: !spec.auto_start,
        root_markers: spec.root_markers.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_mirror_converts() {
        let repr = shuvarie_config::LspConfigRepr {
            disabled: true,
            servers: [(
                "go".to_string(),
                shuvarie_config::LspServerSpecRepr {
                    command: vec!["gopls".to_string()],
                    extensions: vec![".go".to_string()],
                    no_auto_start: true,
                    root_markers: vec!["go.mod".to_string()],
                },
            )]
            .into_iter()
            .collect(),
        };
        let lsp = lsp_config_from_repr(&repr);
        assert!(!lsp.enabled);
        assert_eq!(lsp.resolve().get("go").expect("go").command[0], "gopls");
        let back = lsp_config_to_repr(&lsp);
        assert_eq!(repr, back);
    }

    #[test]
    fn lsp_conversion_is_invertible_on_defaults() {
        let lsp = lsp_config_from_repr(&shuvarie_config::LspConfigRepr::default());
        assert!(lsp.enabled);
        assert!(lsp.servers.is_empty());
        assert_eq!(
            lsp_config_to_repr(&lsp),
            shuvarie_config::LspConfigRepr::default()
        );
    }
}
