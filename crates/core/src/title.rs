//! Session title drafting. The policy comes from `ui.title` in the config:
//! by default the title is (part of) the session's first user prompt, with no
//! LLM involved; `by-llm { … }` opts into the LLM drafting described here —
//! right after a session's first user prompt creates the session, the
//! configured (or active) provider's default small model drafts a title for
//! it in a fire-and-forget side call (no tools, one turn). The provisional
//! heuristic title (`core_task::title_for`) stands until the generated title
//! arrives; the compare-and-swap store write upgrades it without clobbering
//! a manual rename that landed meanwhile. `disabled` skips automatic
//! titling entirely.

use std::sync::{Arc, Mutex};

use shuvarie_config::Connections;
use shuvarie_llm::{ProviderClient, StreamItem, TokenUsage, WorkerRequest};

const DEFAULT_TITLE_PREAMBLE: &str = "\
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

/// The provider connection name and model id a title call should run on: the
/// configured provider/model when given, otherwise the active connection and
/// its catalog's default small model. `None` when the (active or configured)
/// provider connection is unknown, has no catalog entry, or defines no small
/// model — and for an unset provider when no provider is active.
pub fn resolve_model(
    connections: &Connections,
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<(String, String)> {
    let name = match provider {
        Some(name) => name.to_string(),
        None => connections.active.as_ref()?.provider.clone(),
    };
    let pc = connections.providers.get(&name)?;
    if let Some(model) = model {
        return Some((name, model.to_string()));
    }
    let providers = crate::catalog::providers();
    let catalog = crate::catalog::find_provider(&providers, pc.catalog_id()?)?;
    let model = catalog.default_small_model()?.to_string();
    Some((name, model))
}

/// Ask `model` to title the session from the user's first prompt. `preamble`
/// overrides the built-in title system prompt. Returns the sanitized reply,
/// or `None` when the call failed or the reply is empty — the provisional
/// title stands in both cases.
pub async fn generate(
    client: &ProviderClient,
    model: &str,
    preamble: Option<&str>,
    prompt: &str,
) -> Option<String> {
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
        preamble: preamble.unwrap_or(DEFAULT_TITLE_PREAMBLE).to_string(),
        task,
        tools: Vec::new(),
        activity_tx,
        usage: Arc::new(Mutex::new(TokenUsage::default())),
        max_turns: 1,
        context_budget: None,
    };
    let reply = client.run_worker(&req).await.ok()?;
    // Drain activity to avoid backpressure. The receiver is closed first so
    // the drain is bounded by the buffered items: a leaked sender (e.g. one
    // kept alive inside an agent hook) must not hang the title call forever.
    activity_rx.close();
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

    fn connections_with(provider: &str, catalog_id: &str) -> Connections {
        let mut connections = Connections::default();
        connections.providers.insert(
            provider.to_string(),
            ProviderConfig::new(provider, catalog_id, None, None),
        );
        connections
    }

    #[test]
    fn resolve_model_defaults_to_the_active_provider_small_model() {
        let mut connections = connections_with("Anthropic", "anthropic");
        connections.active = Some(Active {
            provider: "Anthropic".to_string(),
            model: Some("claude-sonnet-4-5".to_string()),
            variant: None,
        });
        let (name, model) = resolve_model(&connections, None, None).unwrap();
        assert_eq!(name, "Anthropic");
        // The active model is ignored: the catalog's default small model wins.
        assert_eq!(model, "claude-haiku-4-5-20251001");
    }

    #[test]
    fn resolve_model_honors_the_configured_provider_and_model() {
        let mut connections = connections_with("Anthropic", "anthropic");
        connections.providers.insert(
            "Openai".to_string(),
            ProviderConfig::new("Openai", "openai", None, None),
        );
        connections.active = Some(Active {
            provider: "Anthropic".to_string(),
            model: Some("claude-sonnet-4-5".to_string()),
            variant: None,
        });
        // An explicit provider: its own catalog's default small model.
        let (name, model) = resolve_model(&connections, Some("Openai"), None).unwrap();
        assert_eq!(name, "Openai");
        assert_eq!(model, "gpt-5.6-luna");
        // An explicit model is used verbatim, skipping the catalog.
        let (name, model) =
            resolve_model(&connections, Some("Openai"), Some("gpt-5-nano")).unwrap();
        assert_eq!(name, "Openai");
        assert_eq!(model, "gpt-5-nano");
    }

    #[test]
    fn resolve_model_skips_without_an_active_or_known_provider() {
        assert_eq!(resolve_model(&Connections::default(), None, None), None);
        let connections = connections_with("weird", "not-a-catalog-id");
        assert_eq!(resolve_model(&connections, None, None), None);
        // An unknown configured provider never resolves, active or not.
        assert_eq!(resolve_model(&connections, Some("ghost"), None), None);
        let unlisted = Connections {
            active: Some(Active {
                provider: "ghost".to_string(),
                model: None,
                variant: None,
            }),
            ..Connections::default()
        };
        assert_eq!(resolve_model(&unlisted, None, None), None);
        // But an explicit model needs no catalog entry.
        assert_eq!(
            resolve_model(&connections, Some("weird"), Some("m")),
            Some(("weird".to_string(), "m".to_string()))
        );
    }
}
