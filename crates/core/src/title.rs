//! LLM-based session title generation: right after a session's first user
//! prompt creates the session, the active provider's default small model
//! drafts a title for it in a fire-and-forget side call (no tools, one turn).
//! The provisional heuristic title (`core_task::title_for`) stands until the
//! generated title arrives; the compare-and-swap store write upgrades it
//! without clobbering a manual rename that landed meanwhile.

use std::sync::{Arc, Mutex};

use shuvarie_config::Connections;
use shuvarie_llm::{ProviderClient, StreamItem, TokenUsage, WorkerRequest};

const TITLE_PREAMBLE: &str = "\
You write session titles for an agentic coding assistant. You will be given \
the user's first message in a new session. Reply with a short title that \
names the session's task or topic.\n\n\
Rules:\n\
- Reply with the title only: no quotes, no markup, no explanation, no \
trailing punctuation.\n\
- At most six words, in the language of the user's message.\n\
- Name the task or topic, skipping filler: greetings, \"help me\", \
\"please\", and pleasantries.";

/// How much of the user's first prompt is sent to the title model.
const PROMPT_MAX_CHARS: usize = 2_000;
/// Cap for the generated title.
const TITLE_MAX_CHARS: usize = 64;

/// The active connection's default small model, per the Selune catalog: the
/// provider connection name (so callers reuse the cached client) and the
/// model id. `None` when no provider is active, its catalog entry is
/// unknown, or the catalog defines no models.
pub fn small_model(connections: &Connections) -> Option<(String, String)> {
    let active = connections.active.as_ref()?;
    let pc = connections.providers.get(&active.provider)?;
    let providers = crate::catalog::providers();
    let catalog = crate::catalog::find_provider(&providers, pc.catalog_id()?)?;
    let model = catalog.default_small_model()?.to_string();
    Some((active.provider.clone(), model))
}

/// Ask `model` to title the session from the user's first prompt. Returns
/// the sanitized reply, or `None` when the call failed or the reply is
/// empty — the provisional title stands in both cases.
pub async fn generate(client: &ProviderClient, model: &str, prompt: &str) -> Option<String> {
    let prompt: String = prompt.trim().chars().take(PROMPT_MAX_CHARS).collect();
    if prompt.is_empty() {
        return None;
    }
    let task = format!(
        "Write the session title for the following first message of a new \
         session.\n\n---\n{prompt}\n---"
    );
    let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel::<StreamItem>(16);
    let req = WorkerRequest {
        client: client.clone(),
        name: "title".to_string(),
        model: model.to_string(),
        preamble: TITLE_PREAMBLE.to_string(),
        task,
        tools: Vec::new(),
        activity_tx,
        usage: Arc::new(Mutex::new(TokenUsage::default())),
        max_turns: 1,
        context_budget: None,
    };
    let reply = client.run_worker(&req).await.ok()?;
    // Drain activity to avoid backpressure.
    while activity_rx.recv().await.is_some() {}
    clean(&reply)
}

/// Sanitize a model reply into a single-line title: the first non-empty
/// line, markup stripped, whitespace collapsed, trailing sentence
/// punctuation dropped, capped at [`TITLE_MAX_CHARS`] (trimmed to a word
/// boundary). `None` when nothing usable remains.
fn clean(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let stripped = line
        .trim_matches(|c: char| matches!(c, '"' | '`' | '“' | '”' | '‘' | '’' | '*' | '#' | '-'));
    let mut title: String = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    while title.ends_with(['.', '!', '。', '！']) {
        title.pop();
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        let cut: String = title.chars().take(TITLE_MAX_CHARS).collect();
        title = match cut.rfind(' ') {
            Some(space) if space >= TITLE_MAX_CHARS / 2 => cut[..space].to_string(),
            _ => cut,
        };
    }
    if title.is_empty() { None } else { Some(title) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_config::{Active, ProviderConfig};

    #[test]
    fn clean_takes_the_first_line_and_strips_markup() {
        assert_eq!(
            clean("\"Fix flaky auth retry\"\n\nSome trailing chatter"),
            Some("Fix flaky auth retry".to_string())
        );
        assert_eq!(
            clean("**Refactor db module**"),
            Some("Refactor db module".to_string())
        );
        assert_eq!(clean("- Fix login bug."), Some("Fix login bug".to_string()));
        assert_eq!(clean("Fix   login\tbug"), Some("Fix login bug".to_string()));
    }

    #[test]
    fn clean_caps_long_titles_at_a_word_boundary() {
        let cleaned =
            clean("Fix a very long session title that keeps going and going beyond the cap")
                .unwrap();
        assert!(cleaned.chars().count() <= TITLE_MAX_CHARS);
        assert_eq!(
            cleaned,
            "Fix a very long session title that keeps going and going beyond"
        );
    }

    #[test]
    fn clean_drops_empty_replies() {
        assert_eq!(clean(""), None);
        assert_eq!(clean("   \n  \"\"  "), None);
        assert_eq!(clean("..."), None);
    }

    #[test]
    fn small_model_resolves_the_active_provider_catalog_entry() {
        let mut connections = Connections::default();
        connections.providers.insert(
            "Anthropic".to_string(),
            ProviderConfig::new("Anthropic", "anthropic", None, None),
        );
        connections.active = Some(Active {
            provider: "Anthropic".to_string(),
            model: Some("claude-sonnet-4-5".to_string()),
            variant: None,
        });
        let (name, model) = small_model(&connections).unwrap();
        assert_eq!(name, "Anthropic");
        // The active model is ignored: the catalog's default small model wins.
        assert_eq!(model, "claude-haiku-4-5-20251001");
    }

    #[test]
    fn small_model_skips_without_an_active_provider_or_catalog_entry() {
        assert_eq!(small_model(&Connections::default()), None);
        let mut connections = Connections::default();
        connections.providers.insert(
            "weird".to_string(),
            ProviderConfig::new("Weird", "not-a-catalog-id", None, None),
        );
        connections.active = Some(Active {
            provider: "weird".to_string(),
            model: None,
            variant: None,
        });
        assert_eq!(small_model(&connections), None);
    }
}
