use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::kdl_util::{autoformat, node_error, parse_document};
use super::trusts::{Category, TrustFile, TrustGrants, WorkspaceTrust};
use crate::Result;

pub(crate) const FILE_HEADER: &str = concat!(
    "// Workspace trust decisions, keyed by working directory.\n",
    "// A top-level `trust` block is the default for workspaces without a\n",
    "// record; each `path` record decides for one workspace. Inside `trust`,\n",
    "// `all` grants every category (contexts, skills, configs) and a `trust`\n",
    "// with no entries (or a `path` without one) rejects everything.\n",
    "// Comment a node out with `/-` to remove it.\n",
    "\n",
);

pub(crate) fn from_kdl(contents: &str) -> Result<TrustFile> {
    from_document(&parse_document(contents)?, contents)
}

fn from_document(doc: &KdlDocument, input: &str) -> Result<TrustFile> {
    let mut file = TrustFile::default();
    for node in doc.nodes() {
        match node.name().value() {
            "trust" => {
                if file.default_grants.is_some() {
                    return Err(node_error(input, node, "duplicate `trust` node", None));
                }
                file.default_grants = Some(parse_grants(node, input)?);
            }
            "path" => parse_workspace(node, input, &mut file)?,
            other => {
                return Err(node_error(
                    input,
                    node,
                    format!("unknown node `{other}`"),
                    Some("expected `trust` or `path`".into()),
                ));
            }
        }
    }
    Ok(file)
}

/// The categories granted by a `trust` block's children: bare `all`,
/// `contexts`, `skills`, and `configs` nodes, any mix.
fn parse_grants(node: &KdlNode, input: &str) -> Result<TrustGrants> {
    let children = node
        .children()
        .ok_or_else(|| node_error(input, node, "`trust` requires a block", None))?;
    if !node.entries().is_empty() {
        return Err(node_error(
            input,
            node,
            "`trust` does not take arguments",
            Some("expected: trust { all } or trust { contexts skills configs }".into()),
        ));
    }
    let mut grants = TrustGrants::default();
    for child in children.nodes() {
        if !child.entries().is_empty() || child.children().is_some() {
            return Err(node_error(
                input,
                child,
                format!("`{}` takes no arguments", child.name().value()),
                None,
            ));
        }
        if child.name().value() == "all" {
            if grants.is_all() {
                return Err(node_error(
                    input,
                    child,
                    "duplicate `all` in `trust` block",
                    None,
                ));
            }
            grants = TrustGrants::all();
            continue;
        }
        let Some(category) = Category::parse(child.name().value()) else {
            return Err(node_error(
                input,
                child,
                format!("unknown trust category `{}`", child.name().value()),
                Some("expected `all`, `contexts`, `skills`, or `configs`".into()),
            ));
        };
        if grants.allows(category) {
            return Err(node_error(
                input,
                child,
                format!("duplicate `{}` in `trust` block", category.as_str()),
                None,
            ));
        }
        grants = TrustGrants::from_categories(grants.categories().chain(std::iter::once(category)));
    }
    Ok(grants)
}

fn parse_workspace(node: &KdlNode, input: &str, file: &mut TrustFile) -> Result<()> {
    let mut path = None;
    for entry in node.entries() {
        if entry.name().is_some() {
            return Err(node_error(
                input,
                node,
                "`path` does not take properties",
                Some("expected: path \"<directory>\" { trust { all } }".into()),
            ));
        }
        let KdlValue::String(value) = entry.value() else {
            return Err(node_error(input, node, "`path` must be a string", None));
        };
        if path.is_some() {
            return Err(node_error(
                input,
                node,
                "`path` takes a single directory argument",
                Some("expected: path \"<directory>\" { trust { all } }".into()),
            ));
        }
        path = Some(value.clone());
    }
    let Some(path) = path else {
        return Err(node_error(
            input,
            node,
            "`path` requires a directory argument",
            Some("expected: path \"<directory>\" { trust { all } }".into()),
        ));
    };
    let mut grants = TrustGrants::default();
    let mut saw_trust = false;
    if let Some(children) = node.children() {
        for child in children.nodes() {
            if child.name().value() != "trust" {
                return Err(node_error(
                    input,
                    child,
                    format!("unknown node `{}` in `path` block", child.name().value()),
                    Some("expected `trust`".into()),
                ));
            }
            if saw_trust {
                return Err(node_error(
                    input,
                    child,
                    "duplicate `trust` in `path` block",
                    None,
                ));
            }
            saw_trust = true;
            grants = parse_grants(child, input)?;
        }
    }
    if file.workspaces.iter().any(|w| w.path == path) {
        return Err(node_error(input, node, "duplicate `path` record", None));
    }
    file.workspaces.push(WorkspaceTrust { path, grants });
    Ok(())
}

pub(crate) fn to_kdl(file: &TrustFile) -> Result<String> {
    let mut doc = KdlDocument::new();
    if let Some(default_grants) = &file.default_grants {
        doc.nodes_mut().push(grants_node(default_grants));
    }
    for workspace in &file.workspaces {
        let mut node = KdlNode::new("path");
        node.push(KdlEntry::new(workspace.path.as_str()));
        if !workspace.grants.is_empty() {
            let mut body = KdlDocument::new();
            body.nodes_mut().push(grants_node(&workspace.grants));
            node.set_children(body);
        }
        doc.nodes_mut().push(node);
    }
    autoformat(&mut doc);
    Ok(format!("{FILE_HEADER}{doc}"))
}

fn grants_node(grants: &TrustGrants) -> KdlNode {
    let mut node = KdlNode::new("trust");
    let mut body = KdlDocument::new();
    if grants.covers_all() {
        body.nodes_mut().push(KdlNode::new("all"));
    } else {
        for category in grants.categories() {
            body.nodes_mut().push(KdlNode::new(category.as_str()));
        }
    }
    node.set_children(body);
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusts::ALL_CATEGORIES;

    #[test]
    fn empty_file_is_empty() {
        assert_eq!(from_kdl(" ").unwrap(), TrustFile::default());
    }

    #[test]
    fn parses_planned_format() {
        let text = r#"
            trust {
                /-all
                /-contexts
                /-skills
                /-configs
            }

            path "~/source/my-project/" {
                trust {
                    all
                    /-contexts
                    /-skills
                    /-configs
                }
            }

            /-path "another path" {
                // ...
            }
        "#;
        let parsed = from_kdl(text).unwrap();
        assert_eq!(parsed.default_grants, Some(TrustGrants::default()));
        assert_eq!(parsed.workspaces.len(), 1);
        assert_eq!(parsed.workspaces[0].path, "~/source/my-project/");
        assert!(parsed.workspaces[0].grants.is_all());
    }

    #[test]
    fn global_default_and_records_round_trip() {
        let text = r#"
            trust {
                all
            }
            path "~/source/my-project/" {
                trust {
                    all
                    /-contexts
                }
            }
            path "/tmp/other" {
                trust {
                    contexts
                    skills
                }
            }
            path "/tmp/rejected"
        "#;
        let parsed = from_kdl(text).unwrap();
        assert_eq!(parsed.default_grants, Some(TrustGrants::all()));
        assert!(parsed.workspaces[0].grants.is_all());
        assert_eq!(
            parsed.workspaces[1].grants,
            TrustGrants::from_categories([Category::Contexts, Category::Skills])
        );
        assert!(parsed.workspaces[2].grants.is_empty());

        let saved = to_kdl(&parsed).unwrap();
        let reparsed = from_kdl(&saved).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn all_covers_all_writes_all() {
        let file = TrustFile {
            default_grants: None,
            workspaces: vec![WorkspaceTrust {
                path: "/tmp/x".into(),
                grants: TrustGrants::from_categories(ALL_CATEGORIES),
            }],
        };
        let saved = to_kdl(&file).unwrap();
        assert!(saved.contains("all"), "saved:\n{saved}");
        let reparsed = from_kdl(&saved).unwrap();
        assert!(reparsed.workspaces[0].grants.is_all());
    }

    #[test]
    fn slashdashed_nodes_are_invisible() {
        let text = "path \"a\" { trust { all } }\n/-path \"b\" { trust { all } }";
        let parsed = from_kdl(text).unwrap();
        assert_eq!(parsed.workspaces.len(), 1);
        assert_eq!(parsed.workspaces[0].path, "a");
    }

    #[test]
    fn rejects_unknown_root_node_and_category() {
        let err = from_kdl("pat \"a\"").unwrap_err();
        assert!(err.to_string().contains("unknown node `pat`"), "{err}");

        let err = from_kdl("trust { context }").unwrap_err();
        assert!(err.to_string().contains("unknown trust category"), "{err}");
    }

    #[test]
    fn path_requires_a_string_directory_argument() {
        let err = from_kdl("path").unwrap_err();
        assert!(
            err.to_string().contains("requires a directory argument"),
            "{err}"
        );

        let err = from_kdl("path id=\"a\"").unwrap_err();
        assert!(
            err.to_string().contains("does not take properties"),
            "{err}"
        );

        let err = from_kdl("path 1").unwrap_err();
        assert!(err.to_string().contains("must be a string"), "{err}");

        let err = from_kdl("path \"a\" \"b\"").unwrap_err();
        assert!(
            err.to_string().contains("single directory argument"),
            "{err}"
        );
    }

    #[test]
    fn trust_requires_a_block() {
        let err = from_kdl("trust").unwrap_err();
        assert!(err.to_string().contains("requires a block"), "{err}");
    }

    #[test]
    fn rejects_duplicates() {
        let err = from_kdl("trust { all }\ntrust { skills }").unwrap_err();
        assert!(err.to_string().contains("duplicate `trust` node"), "{err}");

        let err = from_kdl("trust { all\nall }").unwrap_err();
        assert!(err.to_string().contains("duplicate `all`"), "{err}");

        let err = from_kdl("trust { skills\nskills }").unwrap_err();
        assert!(err.to_string().contains("duplicate `skills`"), "{err}");

        let err = from_kdl("path \"a\"\npath \"a\"").unwrap_err();
        assert!(err.to_string().contains("duplicate `path` record"), "{err}");

        let err = from_kdl("path \"a\" {\n    trust { all }\n    trust { all }\n}").unwrap_err();
        assert!(
            err.to_string()
                .contains("duplicate `trust` in `path` block"),
            "{err}"
        );

        let err = from_kdl("path \"a\" { junk }").unwrap_err();
        assert!(err.to_string().contains("unknown node `junk`"), "{err}");

        let err = from_kdl("trust { all \"x\" }").unwrap_err();
        assert!(err.to_string().contains("takes no arguments"), "{err}");
    }

    #[test]
    fn malformed_kdl_has_location() {
        let err = from_kdl("path \"a\" {").unwrap_err();
        let crate::ConfigError::Parse(e) = err else {
            panic!("expected parse error");
        };
        assert!(e.line >= 1);
        assert!(e.column >= 1);
    }
}
