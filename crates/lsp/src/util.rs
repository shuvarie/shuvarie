use std::path::{Path, PathBuf};

pub fn workspace_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn to_file_url(path: &Path) -> Option<lsp_types::Url> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root().join(path)
    };
    lsp_types::Url::from_file_path(&abs).ok()
}

pub fn path_from_url(url: &lsp_types::Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

pub fn language_for_extension(
    specs: &std::collections::BTreeMap<String, crate::config::LspServerSpec>,
    path: &Path,
) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_string();
    for (lang, spec) in specs {
        if spec
            .extensions
            .iter()
            .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&ext))
        {
            return Some(lang.clone());
        }
    }
    None
}

pub fn matches_filter(name: &str, language: &str, filter: &str) -> bool {
    let f = filter.to_lowercase();
    name.to_lowercase().contains(&f) || language.to_lowercase().contains(&f)
}
