use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::LspServerSpec;

pub fn builtin() -> BTreeMap<String, LspServerSpec> {
    let mut m = BTreeMap::new();
    m.insert(
        "rust".into(),
        LspServerSpec {
            command: vec!["rust-analyzer".into()],
            extensions: vec![".rs".into()],
            auto_start: true,
            root_markers: vec!["Cargo.toml".into()],
        },
    );
    m.insert(
        "go".into(),
        LspServerSpec {
            command: vec!["gopls".into()],
            extensions: vec![".go".into()],
            auto_start: true,
            root_markers: vec!["go.mod".into()],
        },
    );
    m.insert(
        "c".into(),
        LspServerSpec {
            command: vec!["clangd".into()],
            extensions: vec![
                ".c".into(),
                ".h".into(),
                ".cpp".into(),
                ".cc".into(),
                ".cxx".into(),
                ".hpp".into(),
                ".hh".into(),
                ".hxx".into(),
            ],
            auto_start: true,
            root_markers: vec![
                "compile_commands.json".into(),
                "CMakeLists.txt".into(),
                "Makefile".into(),
            ],
        },
    );
    m.insert(
        "python".into(),
        LspServerSpec {
            command: vec!["pyright-langserver".into(), "--stdio".into()],
            extensions: vec![".py".into(), ".pyi".into()],
            auto_start: true,
            root_markers: vec![
                "pyproject.toml".into(),
                "setup.py".into(),
                "setup.cfg".into(),
                "requirements.txt".into(),
            ],
        },
    );
    m.insert(
        "typescript".into(),
        LspServerSpec {
            command: vec!["typescript-language-server".into(), "--stdio".into()],
            extensions: vec![
                ".ts".into(),
                ".tsx".into(),
                ".js".into(),
                ".jsx".into(),
                ".mjs".into(),
                ".cjs".into(),
            ],
            auto_start: true,
            root_markers: vec![
                "tsconfig.json".into(),
                "jsconfig.json".into(),
                "package.json".into(),
            ],
        },
    );
    m.insert(
        "zig".into(),
        LspServerSpec {
            command: vec!["zls".into()],
            extensions: vec![".zig".into()],
            auto_start: true,
            root_markers: vec!["build.zig".into()],
        },
    );
    m.insert(
        "lua".into(),
        LspServerSpec {
            command: vec!["lua-language-server".into()],
            extensions: vec![".lua".into()],
            auto_start: true,
            root_markers: vec![".luarc.json".into(), ".luarc.jsonc".into()],
        },
    );
    m.insert(
        "ruby".into(),
        LspServerSpec {
            command: vec!["solargraph".into(), "stdio".into()],
            extensions: vec![".rb".into()],
            auto_start: true,
            root_markers: vec!["Gemfile".into(), "Rakefile".into(), ".ruby-version".into()],
        },
    );
    m
}

pub fn merge_overrides(
    overrides: &BTreeMap<String, LspServerSpec>,
) -> BTreeMap<String, LspServerSpec> {
    let mut merged = builtin();
    for (lang, spec) in overrides {
        merged.insert(lang.clone(), spec.clone());
    }
    merged
}

pub fn probe(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if let Ok(meta) = std::fs::metadata(&candidate)
            && meta.is_file()
        {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_rust_and_go() {
        let b = builtin();
        assert!(b.contains_key("rust"));
        assert!(b.contains_key("go"));
        assert!(b.contains_key("c"));
        assert!(b.contains_key("python"));
        assert!(b.contains_key("typescript"));
    }

    #[test]
    fn merge_keeps_builtins_when_no_override() {
        let empty: BTreeMap<String, LspServerSpec> = BTreeMap::new();
        let merged = merge_overrides(&empty);
        assert_eq!(merged.len(), builtin().len());
    }

    #[test]
    fn probe_finds_a_common_binary() {
        let sh = probe("sh").or_else(|| probe("sh.exe"));
        assert!(sh.is_some(), "expected to find `sh` on PATH");
    }

    #[test]
    fn probe_returns_none_for_missing_binary() {
        let none = probe("definitely-not-a-real-binary-xyz-123");
        assert!(none.is_none());
    }
}
