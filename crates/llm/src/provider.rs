use futures_util::StreamExt;
use selune::ProviderType;

use crate::auth::DeviceCodeHandler;
use crate::message::ChatMsg;
use crate::stream::{StreamItem, StreamStream};
use crate::tool::{DynamicTool, FileChangeHook};
use crate::{LlmError, Result};

#[derive(Debug, Clone)]
pub struct ProviderClient {
    kind: ProviderType,
    base_url: Option<String>,
    list: ListImpl,
}

/// Fused text of a multi-turn agent stream. A paragraph break is inserted
/// where a new request's text resumes after tool activity, so the joined
/// string keeps the separation the streaming view shows at the tool gaps
/// (a single-request run stays byte-identical to its deltas).
#[derive(Default)]
struct TurnText {
    text: String,
    resumed: bool,
}

impl TurnText {
    fn tool_called(&mut self) {
        self.resumed = !self.text.is_empty();
    }

    /// Absorb one text delta; returns the text to forward downstream — the
    /// separator travels inside the stream so every accumulator of the turn
    /// (the TUI's blocks, the core task's interruption buffer) joins the
    /// runs identically.
    fn push(&mut self, delta: String) -> String {
        let separator = std::mem::take(&mut self.resumed);
        if separator {
            self.text.push_str("\n\n");
        }
        self.text.push_str(&delta);
        if separator {
            format!("\n\n{delta}")
        } else {
            delta
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn take(&mut self) -> String {
        std::mem::take(&mut self.text)
    }
}

fn openai_builder(key: &str, base_url: Option<&str>) -> rig_core::providers::openai::ClientBuilder {
    let builder = rig_core::providers::openai::Client::builder().api_key(key);
    if let Some(url) = base_url {
        builder.base_url(url)
    } else {
        builder
    }
}

/// GitHub token formats (`gho_`, `ghp_`, `ghu_`, `ghs_`, `ghr_`, fine-grained
/// `github_pat_`). A pasted Copilot credential that looks like one is a GitHub
/// token to exchange for a Copilot API key, not a Copilot API key itself.
fn is_github_token(token: &str) -> bool {
    ["gho_", "ghp_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| token.starts_with(prefix))
}

/// The ChatGPT subscription backend has no model-listing endpoint; rig's
/// known model constants stand in (the caller enriches them with catalog
/// metadata by id).
fn chatgpt_builtin_models() -> Vec<crate::Model> {
    // ChatGPT's Codex surface has no model-listing endpoint; these are the
    // models OpenAI recommends for subscription sign-in per the official
    // Codex models page (the API-facing rig constants lag behind).
    [
        ("gpt-6-astra", "GPT-6 Astra"),
        ("gpt-5.6-sol", "GPT-5.6 Sol"),
        ("gpt-5.6-terra", "GPT-5.6 Terra"),
        ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ]
    .into_iter()
    .map(|(id, name)| rig_core::model::Model::new(id, name))
    .collect()
}

#[derive(Debug, Clone)]
enum ListImpl {
    OpenAi(rig_core::providers::openai::Client),
    OpenRouter(rig_core::providers::openrouter::Client),
    Anthropic(rig_core::providers::anthropic::Client),
    Gemini(rig_core::providers::gemini::Client),
    Ollama(rig_core::providers::ollama::Client),
    ChatGpt(rig_core::providers::chatgpt::Client),
    Copilot(rig_core::providers::copilot::Client),
    Azure(rig_core::providers::azure::Client),
    Cohere(rig_core::providers::cohere::Client),
    Deepseek(rig_core::providers::deepseek::Client),
    Doubleword(rig_core::providers::doubleword::Client),
    Groq(rig_core::providers::groq::Client),
    Huggingface(rig_core::providers::huggingface::Client),
    Hyperbolic(rig_core::providers::hyperbolic::Client),
    Llamafile(rig_core::providers::llamafile::Client),
    Minimax(rig_core::providers::minimax::Client),
    Mira(rig_core::providers::mira::Client),
    Mistral(rig_core::providers::mistral::Client),
    Moonshot(rig_core::providers::moonshot::Client),
    Perplexity(rig_core::providers::perplexity::Client),
    Together(rig_core::providers::together::Client),
    Venice(rig_core::providers::venice::Client),
    Voyageai(rig_core::providers::voyageai::Client),
    Xai(rig_core::providers::xai::Client),
    Xiaomimimo(rig_core::providers::xiaomimimo::Client),
    Zai(rig_core::providers::zai::Client),
}

/// Build a dedicated rig client for a key-transport provider: `builder()` →
/// `.api_key(key)`, optional `.base_url(url)`, then `build()`. Shared by every
/// provider whose rig module only needs a key and an optional endpoint
/// override; the module path supplies provider-specific defaults and request
/// quirks.
macro_rules! keyed_client {
    ($provider:ident, $variant:ident, $api_key:expr, $base_url:expr) => {{
        let key = $api_key.ok_or(LlmError::Provider("API key required".into()))?;
        let client = rig_core::providers::$provider::Client::builder().api_key(key);
        let client = match $base_url.as_deref() {
            Some(url) => client.base_url(url),
            None => client,
        };
        ListImpl::$variant(
            client
                .build()
                .map_err(|e| LlmError::Provider(e.to_string()))?,
        )
    }};
}

impl ProviderClient {
    pub fn build(
        kind: ProviderType,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Self> {
        Self::build_with_device_code(kind, api_key, base_url, None)
    }

    /// Like [`ProviderClient::build`], with an optional device-code callback
    /// for the OAuth-backed providers (`chatgpt`, `copilot`). With a callback,
    /// signing in without an API key runs the interactive device flow and
    /// surfaces the prompt; without one, a missing token fails fast with an
    /// actionable sign-in error instead of blocking on an unattended flow.
    pub fn build_with_device_code(
        kind: ProviderType,
        api_key: Option<&str>,
        base_url: Option<&str>,
        on_device_code: Option<DeviceCodeHandler>,
    ) -> Result<Self> {
        let base_url = base_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        let list = match kind {
            ProviderType::Openai | ProviderType::OpenaiCompat | ProviderType::Vercel => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = openai_builder(key, base_url.as_deref())
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
            ProviderType::Openrouter => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig_core::providers::openrouter::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::OpenRouter(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Anthropic => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig_core::providers::anthropic::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Anthropic(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Google => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig_core::providers::gemini::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Gemini(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Ollama => {
                let client =
                    rig_core::providers::ollama::Client::builder().api_key(api_key.unwrap_or(""));
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Ollama(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Bedrock | ProviderType::GoogleVertex => {
                // These providers have no rig client (rig 0.42 does not ship
                // bedrock or vertexai transports); fall back to an
                // OpenAI-compatible client at the configured URL.
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = openai_builder(key, base_url.as_deref())
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
            ProviderType::Chatgpt => {
                let mut builder = rig_core::providers::chatgpt::Client::builder();
                if let Some(url) = base_url.as_deref() {
                    builder = builder.base_url(url);
                }
                let builder = match api_key.map(str::trim).filter(|k| !k.is_empty()) {
                    Some(token) => builder.api_key(token),
                    None => builder.oauth(),
                };
                // rig's default merges a generic assistant preamble into every
                // request; shuvarie always supplies its own preamble, so drop
                // it and tag the backend's telemetry with this app.
                let builder = builder.default_instructions("").originator("shuvarie");
                let builder = match on_device_code {
                    Some(handler) => {
                        builder
                            .allow_device_flow(true)
                            .on_device_code(move |prompt| {
                                handler(crate::auth::DeviceCodePrompt {
                                    verification_uri: prompt.verification_uri,
                                    user_code: prompt.user_code,
                                });
                            })
                    }
                    None => builder.allow_device_flow(false),
                };
                ListImpl::ChatGpt(
                    builder
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Copilot => {
                let mut builder = rig_core::providers::copilot::Client::builder();
                if let Some(url) = base_url.as_deref() {
                    builder = builder.base_url(url);
                }
                let builder = match api_key.map(str::trim).filter(|k| !k.is_empty()) {
                    Some(token) if is_github_token(token) => builder.github_access_token(token),
                    Some(key) => builder.api_key(key),
                    None => builder.oauth(),
                };
                let builder = match on_device_code {
                    Some(handler) => {
                        builder
                            .allow_device_flow(true)
                            .on_device_code(move |prompt| {
                                handler(crate::auth::DeviceCodePrompt {
                                    verification_uri: prompt.verification_uri,
                                    user_code: prompt.user_code,
                                });
                            })
                    }
                    None => builder.allow_device_flow(false),
                };
                ListImpl::Copilot(
                    builder
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Azure => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                // rig's Azure extension carries an empty default base URL:
                // the resource endpoint (https://{name}.openai.azure.com) is
                // required on the connection.
                let endpoint = base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .ok_or(LlmError::Provider("Azure endpoint required".into()))?;
                ListImpl::Azure(
                    rig_core::providers::azure::Client::builder()
                        .api_key(key)
                        .azure_endpoint(endpoint.to_string())
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Llamafile => {
                let client = rig_core::providers::llamafile::Client::builder();
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Llamafile(
                    client
                        .api_key(rig_core::client::Nothing)
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Cohere => keyed_client!(cohere, Cohere, api_key, base_url),
            ProviderType::Deepseek => keyed_client!(deepseek, Deepseek, api_key, base_url),
            ProviderType::Doubleword => {
                keyed_client!(doubleword, Doubleword, api_key, base_url)
            }
            ProviderType::Groq => keyed_client!(groq, Groq, api_key, base_url),
            ProviderType::Huggingface => {
                keyed_client!(huggingface, Huggingface, api_key, base_url)
            }
            ProviderType::Hyperbolic => {
                keyed_client!(hyperbolic, Hyperbolic, api_key, base_url)
            }
            ProviderType::Minimax => keyed_client!(minimax, Minimax, api_key, base_url),
            ProviderType::Mira => keyed_client!(mira, Mira, api_key, base_url),
            ProviderType::Mistral => keyed_client!(mistral, Mistral, api_key, base_url),
            ProviderType::Moonshot => keyed_client!(moonshot, Moonshot, api_key, base_url),
            ProviderType::Perplexity => {
                keyed_client!(perplexity, Perplexity, api_key, base_url)
            }
            ProviderType::Together => keyed_client!(together, Together, api_key, base_url),
            ProviderType::Venice => keyed_client!(venice, Venice, api_key, base_url),
            ProviderType::Xai => keyed_client!(xai, Xai, api_key, base_url),
            ProviderType::Xiaomimimo => {
                keyed_client!(xiaomimimo, Xiaomimimo, api_key, base_url)
            }
            ProviderType::Zai => keyed_client!(zai, Zai, api_key, base_url),
            ProviderType::Voyageai => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig_core::providers::voyageai::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Voyageai(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
        };
        Ok(Self {
            kind,
            base_url,
            list,
        })
    }

    /// Run OAuth sign-in to completion for an OAuth-backed provider (`chatgpt`,
    /// `copilot`). Reuses a cached credential when present and valid, refreshes
    /// an expired one, and otherwise runs the interactive device flow — firing
    /// the device-code callback supplied at build time, which surfaces the
    /// verification URL + user code to the user. Resolves once the client holds
    /// a usable token, so it can be awaited off the update loop. Providers
    /// without OAuth sign-in return an error instead of blocking.
    pub async fn authorize(&self) -> Result<()> {
        match &self.list {
            ListImpl::ChatGpt(client) => client
                .authorize()
                .await
                .map_err(|e| LlmError::Provider(e.to_string())),
            ListImpl::Copilot(client) => client
                .authorize()
                .await
                .map_err(|e| LlmError::Provider(e.to_string())),
            _ => Err(LlmError::Provider(format!(
                "{} does not use OAuth sign-in; configure an API key instead",
                self.kind_name()
            ))),
        }
    }

    /// Lowercase kebab-case transport name for user-facing messages.
    fn kind_name(&self) -> &'static str {
        match self.kind {
            ProviderType::Chatgpt => "chatgpt",
            ProviderType::Copilot => "copilot",
            _ => "this provider",
        }
    }

    pub fn kind(&self) -> ProviderType {
        self.kind
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn supports_embeddings(&self) -> bool {
        matches!(
            self.kind,
            ProviderType::Openai
                | ProviderType::OpenaiCompat
                | ProviderType::Openrouter
                | ProviderType::Google
                | ProviderType::Ollama
                | ProviderType::Vercel
                | ProviderType::Copilot
                | ProviderType::Azure
                | ProviderType::Cohere
                | ProviderType::Doubleword
                | ProviderType::Llamafile
                | ProviderType::Mistral
                | ProviderType::Together
                | ProviderType::Venice
                | ProviderType::Voyageai
        )
    }

    pub async fn embed(&self, model: &str, dims: usize, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        match &self.list {
            ListImpl::OpenAi(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::OpenRouter(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Gemini(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Ollama(c) => embed_via(c, model, Some(dims), texts.to_vec()).await,
            ListImpl::Copilot(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Azure(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Cohere(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Doubleword(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Llamafile(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Mistral(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Together(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Venice(c) => embed_via(c, model, None, texts.to_vec()).await,
            ListImpl::Voyageai(c) => embed_via(c, model, None, texts.to_vec()).await,
            _ => Err(LlmError::Embedding(
                "provider does not support embeddings".into(),
            )),
        }
    }

    pub async fn list_models(&self) -> Result<Vec<crate::Model>> {
        use rig_core::client::ModelListingClient;

        let models = match &self.list {
            ListImpl::OpenAi(c) => c.list_models().await,
            ListImpl::OpenRouter(c) => c.list_models().await,
            ListImpl::Anthropic(c) => c.list_models().await,
            ListImpl::Gemini(c) => c.list_models().await,
            ListImpl::Ollama(c) => c.list_models().await,
            ListImpl::ChatGpt(_) => {
                return Ok(chatgpt_builtin_models());
            }
            ListImpl::Copilot(c) => c.list_models().await,
            ListImpl::Deepseek(c) => c.list_models().await,
            ListImpl::Groq(c) => c.list_models().await,
            ListImpl::Minimax(c) => c.list_models().await,
            ListImpl::Mira(c) => c.list_models().await,
            ListImpl::Mistral(c) => c.list_models().await,
            ListImpl::Moonshot(c) => c.list_models().await,
            ListImpl::Venice(c) => c.list_models().await,
            ListImpl::Xiaomimimo(c) => c.list_models().await,
            ListImpl::Azure(_)
            | ListImpl::Cohere(_)
            | ListImpl::Doubleword(_)
            | ListImpl::Huggingface(_)
            | ListImpl::Hyperbolic(_)
            | ListImpl::Llamafile(_)
            | ListImpl::Perplexity(_)
            | ListImpl::Together(_)
            | ListImpl::Voyageai(_)
            | ListImpl::Xai(_)
            | ListImpl::Zai(_) => {
                return Err(LlmError::Model(
                    "provider does not support model listing".into(),
                ));
            }
        }
        .map_err(|e| LlmError::Model(e.to_string()))?;

        Ok(models.data)
    }

    pub async fn run_worker(
        &self,
        req: &crate::agent::WorkerRequest,
    ) -> std::result::Result<String, String> {
        match &self.list {
            ListImpl::OpenAi(c) => worker_via(c, req).await,
            ListImpl::OpenRouter(c) => worker_via(c, req).await,
            ListImpl::Anthropic(c) => worker_via(c, req).await,
            ListImpl::Gemini(c) => worker_via(c, req).await,
            ListImpl::Ollama(c) => worker_via(c, req).await,
            ListImpl::ChatGpt(c) => worker_via(c, req).await,
            ListImpl::Copilot(c) => worker_via(c, req).await,
            ListImpl::Azure(c) => worker_via(c, req).await,
            ListImpl::Cohere(c) => worker_via(c, req).await,
            ListImpl::Deepseek(c) => worker_via(c, req).await,
            ListImpl::Doubleword(c) => worker_via(c, req).await,
            ListImpl::Groq(c) => worker_via(c, req).await,
            ListImpl::Huggingface(c) => worker_via(c, req).await,
            ListImpl::Hyperbolic(c) => worker_via(c, req).await,
            ListImpl::Llamafile(c) => worker_via(c, req).await,
            ListImpl::Minimax(c) => worker_via(c, req).await,
            ListImpl::Mira(c) => worker_via(c, req).await,
            ListImpl::Mistral(c) => worker_via(c, req).await,
            ListImpl::Moonshot(c) => worker_via(c, req).await,
            ListImpl::Perplexity(c) => worker_via(c, req).await,
            ListImpl::Together(c) => worker_via(c, req).await,
            ListImpl::Venice(c) => worker_via(c, req).await,
            ListImpl::Xai(c) => worker_via(c, req).await,
            ListImpl::Xiaomimimo(c) => worker_via(c, req).await,
            ListImpl::Zai(c) => worker_via(c, req).await,
            ListImpl::Voyageai(_) => {
                Err("voyageai supports embeddings only, not completions".to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stream(
        &self,
        model: &str,
        preamble: Option<&str>,
        prompt: &str,
        history: &[ChatMsg],
        tools: Vec<DynamicTool>,
        workers: &mut [crate::agent::WorkerAgent],
        max_turns: usize,
        context_budget: Option<crate::context_hook::ContextBudget>,
    ) -> StreamStream {
        let tracker = crate::context_hook::UsageTracker::new();
        let tracker_for_hook = tracker.clone();
        let user_msg = rig_core::message::Message::user(prompt.to_string());
        let rig_history: Vec<rig_core::message::Message> = history
            .iter()
            .cloned()
            .map(rig_core::message::Message::from)
            .collect();
        let mut dynamic = tools;
        for worker in workers.iter() {
            dynamic.push(crate::tool::into_dynamic(worker.name(), worker.clone()));
        }
        let worker_names: std::collections::HashSet<String> =
            workers.iter().map(|w| w.name().to_string()).collect();
        let mut receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>> = workers
            .iter_mut()
            .filter_map(crate::agent::WorkerAgent::take_activity_receiver)
            .collect();
        let (early_tx, early_rx) = tokio::sync::mpsc::channel(64);
        let file_hook =
            FileChangeHook::new().with_early_finish(worker_names.clone(), None, early_tx);
        receivers.push(early_rx);

        match &self.list {
            ListImpl::OpenAi(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::OpenRouter(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Anthropic(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Gemini(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Ollama(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::ChatGpt(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Copilot(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Azure(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Cohere(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Deepseek(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Doubleword(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Groq(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Huggingface(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Hyperbolic(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Llamafile(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Minimax(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Mira(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Mistral(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Moonshot(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Perplexity(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Together(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Venice(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Xai(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Xiaomimimo(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Zai(c) => {
                stream_via(
                    c,
                    model,
                    preamble,
                    dynamic,
                    context_budget,
                    tracker_for_hook,
                    file_hook,
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Voyageai(_) => Box::pin(futures_util::stream::iter([StreamItem::Error {
                message: "voyageai supports embeddings only, not completions".into(),
                reason: "Turn error".into(),
            }])),
        }
    }
}

/// Embed `texts` through any rig [`EmbeddingsClient`], converting to `f32`
/// vectors. `ndims` targets providers whose embedding dimension is chosen
/// per request (e.g. Ollama).
async fn embed_via<C>(
    client: &C,
    model: &str,
    ndims: Option<usize>,
    texts: Vec<String>,
) -> Result<Vec<Vec<f32>>>
where
    C: rig_core::client::EmbeddingsClient,
    C::EmbeddingModel: rig_core::embeddings::EmbeddingModel,
{
    use rig_core::embeddings::EmbeddingsBuilder;

    let model = match ndims {
        Some(dims) => client.embedding_model_with_ndims(model, dims),
        None => client.embedding_model(model),
    };
    let embeddings = EmbeddingsBuilder::new(model)
        .documents(texts)
        .map_err(|e| LlmError::Embedding(e.to_string()))?
        .build()
        .await
        .map_err(|e| LlmError::Embedding(e.to_string()))?;
    Ok(embeddings
        .into_iter()
        .flat_map(|(_, embeddings)| embeddings)
        .map(|embedding| embedding.vec.iter().map(|&v| v as f32).collect())
        .collect())
}

/// Run a worker-agent request against any completion-capable rig client.
async fn worker_via<C>(
    client: &C,
    req: &crate::agent::WorkerRequest,
) -> std::result::Result<String, String>
where
    C: rig_core::client::CompletionClient + rig_agent::client::AgentClientExt,
    C::CompletionModel: 'static,
{
    let user_msg = rig_core::message::Message::user(req.task.clone());
    let activity_tx = req.activity_tx.clone();
    let usage = std::sync::Arc::clone(&req.usage);
    let tracker = crate::context_hook::UsageTracker::new();
    let file_hook = FileChangeHook::new().with_early_finish(
        std::collections::HashSet::new(),
        Some(req.name.clone()),
        activity_tx.clone(),
    );
    let agent = agent_with_tools(
        client,
        &req.model,
        Some(&req.preamble),
        req.tools.clone(),
        req.context_budget.clone(),
        tracker,
        file_hook.clone(),
    );
    run_worker_agent(
        agent,
        &req.name,
        user_msg,
        activity_tx,
        usage,
        req.max_turns,
        file_hook,
    )
    .await
}

/// Stream a chat turn through any completion-capable rig client.
#[allow(clippy::too_many_arguments)]
async fn stream_via<C>(
    client: &C,
    model: &str,
    preamble: Option<&str>,
    dynamic: Vec<DynamicTool>,
    context_budget: Option<crate::context_hook::ContextBudget>,
    tracker_for_hook: std::sync::Arc<crate::context_hook::UsageTracker>,
    file_hook: FileChangeHook,
    user_msg: rig_core::message::Message,
    rig_history: Vec<rig_core::message::Message>,
    receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>>,
    worker_names: std::collections::HashSet<String>,
    max_turns: usize,
    tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
) -> StreamStream
where
    C: rig_core::client::CompletionClient + rig_agent::client::AgentClientExt,
    C::CompletionModel: 'static,
{
    use rig_agent::streaming::StreamingChat;

    let agent = agent_with_tools(
        client,
        model,
        preamble,
        dynamic,
        context_budget,
        tracker_for_hook,
        file_hook.clone(),
    );
    let stream = agent
        .stream_chat(user_msg, rig_history)
        .max_turns(max_turns)
        .await;
    map_agent_stream(stream, receivers, worker_names, tracker, file_hook)
}

#[allow(clippy::too_many_arguments)]
fn agent_with_tools<C>(
    client: &C,
    model: &str,
    preamble: Option<&str>,
    dynamic: Vec<DynamicTool>,
    context_budget: Option<crate::context_hook::ContextBudget>,
    tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
    file_hook: FileChangeHook,
) -> rig_agent::agent::Agent
where
    C: rig_core::client::CompletionClient + rig_agent::client::AgentClientExt,
    C::CompletionModel: 'static,
{
    let builder = match preamble {
        Some(p) => client.agent(model).preamble(p),
        None => client.agent(model).without_preamble(),
    };
    if let Some(budget) = context_budget {
        builder
            .dynamic_tools(dynamic)
            .add_hook(crate::context_hook::ContextHook::new(budget, tracker))
            .add_hook(file_hook)
            .build()
    } else {
        builder.dynamic_tools(dynamic).add_hook(file_hook).build()
    }
}

async fn run_worker_agent(
    agent: rig_agent::agent::Agent,
    name: &str,
    prompt: rig_core::message::Message,
    activity_tx: tokio::sync::mpsc::Sender<StreamItem>,
    usage: std::sync::Arc<std::sync::Mutex<crate::TokenUsage>>,
    max_turns: usize,
    file_hook: FileChangeHook,
) -> std::result::Result<String, String> {
    use rig_agent::streaming::StreamingChat;

    let mut stream = agent
        .stream_chat(prompt, Vec::<rig_core::message::Message>::new())
        .max_turns(max_turns)
        .await;

    let mut turn_text = TurnText::default();
    let mut usage_aggregate = crate::TokenUsage::default();
    let mut tool_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
                rig_core::streaming::StreamedAssistantContent::Text(t),
            )) => {
                turn_text.push(t.text);
            }
            Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
                rig_core::streaming::StreamedAssistantContent::ToolCall {
                    tool_call,
                    internal_call_id,
                },
            )) => {
                tool_names.insert(internal_call_id.clone(), tool_call.function.name.clone());
                let _ = activity_tx
                    .send(StreamItem::ToolStart {
                        name: tool_call.function.name,
                        args: tool_call.function.arguments,
                        worker: Some(name.to_string()),
                        call_id: internal_call_id,
                    })
                    .await;
            }
            Ok(rig_agent::agent::MultiTurnStreamItem::StreamUserItem(
                rig_core::streaming::StreamedUserContent::ToolResult {
                    tool_result,
                    internal_call_id,
                },
            )) if !file_hook.surfaced_early(&internal_call_id) => {
                let tool_name = tool_names.remove(&internal_call_id).unwrap_or_default();
                let captured = file_hook.take(&internal_call_id);
                let mut output = String::new();
                for content in tool_result.content.iter() {
                    if let Some(text) = content.as_text() {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(text);
                    }
                }
                let mut ok = !captured.failed;
                if output.is_empty() {
                    ok = false;
                    output = String::from("(no output)");
                }
                let _ = activity_tx
                    .send(StreamItem::ToolResult {
                        name: tool_name,
                        output,
                        ok,
                        worker: Some(name.to_string()),
                        file_change: captured.file_change,
                        streams: captured.shell,
                        call_id: internal_call_id,
                    })
                    .await;
            }
            Ok(rig_agent::agent::MultiTurnStreamItem::CompletionCall(call)) => {
                let _ = activity_tx
                    .send(StreamItem::Usage {
                        usage: call.usage,
                        worker: Some(name.to_string()),
                    })
                    .await;
            }
            Ok(rig_agent::agent::MultiTurnStreamItem::FinalResponse(resp)) => {
                usage_aggregate = resp.usage;
            }
            Ok(_) => {}
            Err(e) => {
                let message = format!("worker '{name}' failed: {e}");
                let _ = activity_tx
                    .send(StreamItem::Error {
                        message: message.clone(),
                        reason: "Worker failed".to_string(),
                    })
                    .await;
                return Err(message);
            }
        }
    }
    {
        let mut guard = usage.lock().unwrap();
        guard.input_tokens += usage_aggregate.input_tokens;
        guard.output_tokens += usage_aggregate.output_tokens;
        guard.total_tokens += usage_aggregate.total_tokens;
        guard.cached_input_tokens += usage_aggregate.cached_input_tokens;
        guard.reasoning_tokens += usage_aggregate.reasoning_tokens;
    }
    Ok(turn_text.take())
}

/// Map rig's agent stream onto shuvarie's `StreamItem`s, merging in the
/// worker-activity receivers. The `file_hook`'s early-finish channel, when
/// wired, surfaces each tool result the moment its call completes; the
/// buffered rig results of those calls are dropped here.
fn map_agent_stream(
    stream: rig_agent::agent::StreamingResult,
    receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>>,
    worker_names: std::collections::HashSet<String>,
    tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
    file_hook: FileChangeHook,
) -> StreamStream {
    let mut tool_called = false;
    let mut turn_text = TurnText::default();
    let mut tool_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut pending_workers: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let tracker_clone = tracker.clone();
    let main = stream.map(move |item| match item {
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
            rig_core::streaming::StreamedAssistantContent::Text(t),
        )) => StreamItem::Delta {
            text: turn_text.push(t.text),
        },
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
            rig_core::streaming::StreamedAssistantContent::Reasoning { reasoning, .. },
        )) => {
            let text = reasoning.display_text();
            if text.is_empty() {
                StreamItem::Delta {
                    text: String::new(),
                }
            } else {
                StreamItem::Reasoning { text }
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
            rig_core::streaming::StreamedAssistantContent::ReasoningDelta { reasoning, .. },
        )) => {
            if reasoning.is_empty() {
                StreamItem::Delta {
                    text: String::new(),
                }
            } else {
                StreamItem::Reasoning { text: reasoning }
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
            rig_core::streaming::StreamedAssistantContent::ToolCall {
                tool_call,
                internal_call_id,
            },
        )) => {
            tool_called = true;
            turn_text.tool_called();
            let name = tool_call.function.name.clone();
            if worker_names.contains(&name) {
                pending_workers.push_back(name.clone());
                StreamItem::WorkerStart {
                    name,
                    args: tool_call.function.arguments,
                    call_id: internal_call_id,
                }
            } else {
                tool_names.insert(internal_call_id.clone(), name.clone());
                StreamItem::ToolStart {
                    name,
                    args: tool_call.function.arguments,
                    worker: None,
                    call_id: internal_call_id,
                }
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamUserItem(
            rig_core::streaming::StreamedUserContent::ToolResult {
                tool_result: _,
                internal_call_id,
            },
        )) if file_hook.surfaced_early(&internal_call_id) => StreamItem::Delta {
            text: String::new(),
        },
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamUserItem(
            rig_core::streaming::StreamedUserContent::ToolResult {
                tool_result,
                internal_call_id,
            },
        )) => {
            let mut output = String::new();
            for content in tool_result.content.iter() {
                if let Some(text) = content.as_text() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(text);
                }
            }
            let captured = file_hook.take(&internal_call_id);
            let mut ok = !captured.failed;
            if output.is_empty() {
                ok = false;
                output = String::from("(no output)");
            }
            let name = tool_names.remove(&internal_call_id);
            match name {
                Some(name) => StreamItem::ToolResult {
                    name,
                    output,
                    ok,
                    worker: None,
                    file_change: captured.file_change,
                    streams: captured.shell,
                    call_id: internal_call_id,
                },
                None => StreamItem::WorkerResult {
                    name: pending_workers.pop_front().unwrap_or_default(),
                    output,
                    ok,
                    call_id: internal_call_id,
                },
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::FinalResponse(resp)) => {
            let text = if turn_text.is_empty() && tool_called {
                String::new()
            } else {
                turn_text.take()
            };
            StreamItem::Done {
                text,
                usage: resp.usage,
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::CompletionCall(call)) => {
            tracker_clone.record(call.usage);
            StreamItem::Usage {
                usage: call.usage,
                worker: None,
            }
        }
        Ok(_) => StreamItem::Delta {
            text: String::new(),
        },
        Err(e) => {
            let message = e.to_string();
            if message.contains(crate::context_hook::OVERFLOW_REASON) {
                StreamItem::Overflow
            } else {
                // Every error from the turn retries: connection failures
                // keep their specific label, everything else falls back to
                // the generic one.
                let reason = crate::retry::classify_connection_error(&e)
                    .map(|failure| failure.reason)
                    .unwrap_or_else(|| "Turn error".to_string());
                StreamItem::Error { message, reason }
            }
        }
    });
    Box::pin(merge_streams(main, receivers))
}

fn merge_streams(
    main: impl futures_core::Stream<Item = StreamItem> + Send + 'static,
    receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>>,
) -> StreamStream {
    use futures_util::stream::select_all;
    use std::pin::Pin;

    let mut streams: Vec<Pin<Box<dyn futures_core::Stream<Item = StreamItem> + Send>>> = Vec::new();
    streams.push(Box::pin(main));
    for rx in receivers {
        streams.push(Box::pin(tokio_rx_stream(rx)));
    }
    Box::pin(select_all(streams))
}

fn tokio_rx_stream(
    rx: tokio::sync::mpsc::Receiver<StreamItem>,
) -> impl futures_core::Stream<Item = StreamItem> {
    use futures_util::stream::unfold;
    unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use futures_util::StreamExt;
    use serde_json::json;

    fn pending_stream() -> impl futures_core::Stream<Item = StreamItem> + Send + 'static {
        futures_util::stream::pending::<StreamItem>()
    }

    #[test]
    fn turn_text_single_request_stays_byte_identical() {
        let mut turn = TurnText::default();
        assert_eq!(turn.push("Hello ".into()), "Hello ");
        assert_eq!(turn.push("world".into()), "world");
        assert_eq!(turn.take(), "Hello world");
    }

    #[test]
    fn turn_text_joins_runs_after_tool_with_paragraph_break() {
        let mut turn = TurnText::default();
        assert_eq!(turn.push("Run one.".into()), "Run one.");
        turn.tool_called();
        assert_eq!(turn.push("Run two.".into()), "\n\nRun two.");
        assert_eq!(turn.take(), "Run one.\n\nRun two.");
    }

    #[test]
    fn turn_text_tool_only_prefix_gets_no_leading_break() {
        let mut turn = TurnText::default();
        turn.tool_called();
        assert_eq!(turn.push("First text.".into()), "First text.");
        turn.tool_called();
        assert_eq!(turn.push("Second text.".into()), "\n\nSecond text.");
        assert_eq!(turn.take(), "First text.\n\nSecond text.");
    }

    #[test]
    fn turn_text_rearms_between_every_run() {
        let mut turn = TurnText::default();
        turn.push("a".into());
        turn.tool_called();
        turn.push("b".into());
        turn.tool_called();
        turn.push("c".into());
        assert_eq!(turn.take(), "a\n\nb\n\nc");
    }

    #[tokio::test]
    async fn merge_streams_yields_main_and_worker_items() {
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(8);
        let main_stream = futures_util::stream::iter(vec![StreamItem::Delta {
            text: "main".into(),
        }]);
        let mut merged = merge_streams(main_stream, vec![worker_rx]);
        let worker_items = [
            StreamItem::ToolStart {
                name: "read_file".into(),
                args: json!({ "path": "x.rs" }),
                worker: Some("explore_workspace".into()),
                call_id: "w1".into(),
            },
            StreamItem::ToolResult {
                name: "read_file".into(),
                output: "ok".into(),
                ok: true,
                worker: Some("explore_workspace".into()),
                file_change: None,
                streams: None,
                call_id: "w1".into(),
            },
        ];
        let _ = worker_tx.send(worker_items[0].clone()).await;
        let _ = worker_tx.send(worker_items[1].clone()).await;
        drop(worker_tx);
        let mut collected = Vec::new();
        while let Some(item) = merged.next().await {
            collected.push(item);
        }
        assert_eq!(collected.len(), 3, "main delta + two worker items");
        assert!(collected.contains(&worker_items[0]));
        assert!(collected.contains(&worker_items[1]));
        assert!(collected.contains(&StreamItem::Delta {
            text: "main".into()
        }));
    }

    #[tokio::test]
    async fn merge_streams_drains_workers_after_main_done() {
        // `select_all` is round-robin but polls the main stream first, so a
        // worker item queued at the moment `Done` arrives is yielded after
        // it. The merged stream must keep yielding until the receiver
        // drains — the core only sends `Event::StreamDone` once it returns
        // `None`.
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(8);
        let done = StreamItem::Done {
            text: "final".into(),
            usage: crate::TokenUsage::default(),
        };
        let worker_item = StreamItem::ToolResult {
            name: "grep".into(),
            output: "found".into(),
            ok: true,
            worker: Some("explore_workspace".into()),
            file_change: None,
            streams: None,
            call_id: "w2".into(),
        };
        let _ = worker_tx.send(worker_item.clone()).await;
        drop(worker_tx);
        let mut merged = merge_streams(
            futures_util::stream::iter(vec![done.clone()]),
            vec![worker_rx],
        );
        let mut collected = Vec::new();
        while let Some(item) = merged.next().await {
            collected.push(item);
        }
        assert_eq!(
            collected,
            vec![done, worker_item],
            "worker items must trail Done, never be dropped"
        );
    }

    #[tokio::test]
    async fn merge_streams_emits_worker_items_even_when_main_pending() {
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(8);
        let mut merged = merge_streams(pending_stream(), vec![worker_rx]);
        let item = StreamItem::WorkerStart {
            name: "run_tests".into(),
            args: json!({ "task": "run tests" }),
            call_id: "w3".into(),
        };
        let _ = worker_tx.send(item.clone()).await;
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), merged.next())
            .await
            .expect("worker item should arrive")
            .expect("stream should not end");
        assert_eq!(got, item);
    }

    #[tokio::test]
    async fn worker_missing_task_returns_error() {
        let client = ProviderClient::build(selune::ProviderType::Ollama, None, None).unwrap();
        let (_activity_tx, _activity_rx) = tokio::sync::mpsc::channel::<StreamItem>(8);
        let usage = std::sync::Arc::new(std::sync::Mutex::new(crate::TokenUsage::default()));
        let worker = crate::agent::WorkerAgent::new(
            "test_worker",
            "a test worker",
            "you are a test worker",
            client,
            "test-model",
            Vec::new(),
            usage,
            10,
            None,
        );
        let err = worker
            .call(&mut crate::tool::ToolContext::new(), json!({}))
            .await
            .expect_err("missing task should fail");
        assert!(
            err.to_string().contains("task"),
            "error should mention task: {err}"
        );
    }

    #[test]
    fn supports_embeddings_by_provider() {
        use selune::ProviderType::*;
        let ok = [
            Openai,
            OpenaiCompat,
            Openrouter,
            Google,
            Ollama,
            Vercel,
            Copilot,
        ];
        let no = [Anthropic, Bedrock, GoogleVertex, Chatgpt];
        for p in ok {
            let client = ProviderClient::build(p, Some("k"), None).unwrap();
            assert!(
                client.supports_embeddings(),
                "{p:?} should support embeddings"
            );
        }
        for p in no {
            let client = ProviderClient::build(p, Some("k"), None).unwrap();
            assert!(!client.supports_embeddings(), "{p:?} should not");
        }
        // Azure and the embeddings-capable transports from the rig survey;
        // Azure's resource endpoint is required at build time.
        let azure =
            ProviderClient::build(Azure, Some("k"), Some("https://res.openai.azure.com")).unwrap();
        assert!(azure.supports_embeddings());
        for p in [
            Cohere, Doubleword, Llamafile, Mistral, Together, Venice, Voyageai,
        ] {
            let client = ProviderClient::build(p, Some("k"), None).unwrap();
            assert!(
                client.supports_embeddings(),
                "{p:?} should support embeddings"
            );
        }
    }

    #[test]
    fn auth_backed_clients_build_with_and_without_a_key() {
        use selune::ProviderType::*;
        for kind in [Chatgpt, Copilot] {
            let keyed = ProviderClient::build(kind, Some("tok"), None).unwrap();
            assert_eq!(keyed.kind(), kind);
            let oauth = ProviderClient::build(kind, None, None).unwrap();
            assert_eq!(oauth.kind(), kind);
            let handler: crate::auth::DeviceCodeHandler = std::sync::Arc::new(|_| {});
            let prompted =
                ProviderClient::build_with_device_code(kind, None, None, Some(handler)).unwrap();
            assert_eq!(prompted.kind(), kind);
        }
    }

    #[test]
    fn github_token_prefixes_route_to_the_exchange_flow() {
        for token in ["gho_x", "ghp_x", "ghu_x", "ghs_x", "ghr_x", "github_pat_x"] {
            assert!(is_github_token(token), "{token}");
        }
        for key in ["sk-x", "copilot-key", ""] {
            assert!(!is_github_token(key), "{key}");
        }
    }

    #[tokio::test]
    async fn authorize_with_a_static_key_resolves_without_network() {
        use selune::ProviderType::*;
        // ChatGPT's AccessToken source and Copilot's plain API-key source both
        // answer auth_context() directly — no HTTP, no cache files touched.
        let chatgpt = ProviderClient::build(Chatgpt, Some("tok"), None).unwrap();
        chatgpt
            .authorize()
            .await
            .expect("static access token authorizes");
        let copilot = ProviderClient::build(Copilot, Some("copilot-key"), None).unwrap();
        copilot
            .authorize()
            .await
            .expect("static api key authorizes");
    }

    #[tokio::test]
    async fn authorize_rejects_non_oauth_kinds() {
        let client = ProviderClient::build(selune::ProviderType::Openai, Some("k"), None).unwrap();
        let err = client.authorize().await.expect_err("no OAuth for openai");
        assert!(
            err.to_string().contains("OAuth sign-in"),
            "error should explain the limitation: {err}"
        );
    }

    #[test]
    fn chatgpt_builtin_models_carry_the_codex_recommended_set() {
        let models = chatgpt_builtin_models();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"gpt-6-astra"), "{ids:?}");
        assert!(ids.contains(&"gpt-5.6-sol"), "{ids:?}");
        assert!(models.iter().all(|m| m.name.is_some()), "{ids:?}");
    }

    #[tokio::test]
    async fn chatgpt_list_models_returns_builtin_constants() {
        let client = ProviderClient::build(selune::ProviderType::Chatgpt, None, None).unwrap();
        let models = client.list_models().await.unwrap();
        assert!(!models.is_empty());
    }

    #[tokio::test]
    async fn embed_unsupported_provider_errors() {
        let client =
            ProviderClient::build(selune::ProviderType::Anthropic, Some("k"), None).unwrap();
        let err = client
            .embed("some-model", 768, &["hello".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Embedding(_)));
    }

    #[tokio::test]
    async fn embed_empty_input_returns_empty() {
        let client = ProviderClient::build(selune::ProviderType::Ollama, None, None).unwrap();
        let out = client.embed("m", 384, &[]).await.unwrap();
        assert!(out.is_empty());
    }
    mod early_tool_results {
        use super::*;
        use crate::tool::{ToolContext, ToolExecutionError, ToolOutput, into_dynamic};
        use rig_agent::agent::AgentBuilder;
        use rig_agent::streaming::StreamingChat;
        use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
        use std::collections::HashSet;

        struct InstantTool;

        impl Tool for InstantTool {
            const NAME: &'static str = "instant";
            type Args = serde_json::Value;
            type Output = ToolOutput;
            type Error = ToolExecutionError;

            fn description(&self) -> String {
                "finishes immediately".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn call(
                &self,
                _ctx: &mut ToolContext,
                _args: Self::Args,
            ) -> std::result::Result<Self::Output, Self::Error> {
                Ok::<ToolOutput, ToolExecutionError>(ToolOutput::text("instant done"))
            }
        }

        /// A tool that blocks until released, so a batch mate's result has a
        /// window to surface first.
        #[derive(Clone)]
        struct ControlledTool {
            release: std::sync::Arc<tokio::sync::Notify>,
        }

        impl ControlledTool {
            fn new(release: std::sync::Arc<tokio::sync::Notify>) -> Self {
                Self { release }
            }
        }

        impl Tool for ControlledTool {
            const NAME: &'static str = "controlled";
            type Args = serde_json::Value;
            type Output = ToolOutput;
            type Error = ToolExecutionError;

            fn description(&self) -> String {
                "waits for release".to_string()
            }

            fn parameters(&self) -> serde_json::Value {
                json!({"type": "object", "properties": {}})
            }

            async fn call(
                &self,
                _ctx: &mut ToolContext,
                _args: Self::Args,
            ) -> std::result::Result<Self::Output, Self::Error> {
                self.release.notified().await;
                Ok::<ToolOutput, ToolExecutionError>(ToolOutput::text("controlled done"))
            }
        }

        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

        fn batch_model(slow_tool: &str) -> MockCompletionModel {
            MockCompletionModel::from_stream_turns([
                vec![
                    MockStreamEvent::tool_call("call-instant", "instant", json!({})),
                    MockStreamEvent::tool_call("call-controlled", slow_tool, json!({})),
                    MockStreamEvent::final_response_with_default_usage(),
                ],
                vec![
                    MockStreamEvent::text("done"),
                    MockStreamEvent::final_response_with_default_usage(),
                ],
            ])
        }

        async fn agent_stream(
            model: MockCompletionModel,
            tools: Vec<DynamicTool>,
            worker_names: HashSet<String>,
        ) -> StreamStream {
            let (early_tx, early_rx) = tokio::sync::mpsc::channel(16);
            let file_hook =
                FileChangeHook::new().with_early_finish(worker_names.clone(), None, early_tx);
            let agent = AgentBuilder::new(model)
                .dynamic_tools(tools)
                .add_hook(file_hook.clone())
                .build();
            let stream = agent
                .stream_chat(
                    rig_core::message::Message::user("go"),
                    Vec::<rig_core::message::Message>::new(),
                )
                .max_turns(2)
                .await;
            map_agent_stream(
                stream,
                vec![early_rx],
                worker_names,
                crate::context_hook::UsageTracker::new(),
                file_hook,
            )
        }

        fn results_named(items: &[StreamItem], name: &str) -> usize {
            items
                .iter()
                .filter(|item| matches!(item, StreamItem::ToolResult { name: n, .. } if n == name))
                .count()
        }

        fn all_results(items: &[StreamItem]) -> usize {
            items
                .iter()
                .filter(|item| matches!(item, StreamItem::ToolResult { .. }))
                .count()
        }

        #[tokio::test]
        async fn fast_tool_result_surfaces_while_slow_tool_still_runs() {
            let release = std::sync::Arc::new(tokio::sync::Notify::new());
            let tools = vec![
                into_dynamic("instant", InstantTool),
                into_dynamic("controlled", ControlledTool::new(release.clone())),
            ];
            let mut stream = agent_stream(batch_model("controlled"), tools, HashSet::new()).await;

            let mut items = Vec::new();
            loop {
                let item = tokio::time::timeout(TIMEOUT, stream.next())
                    .await
                    .expect("stream stalled before the slow tool started")
                    .expect("stream ended before the slow tool started");
                if matches!(&item, StreamItem::ToolStart { name, .. } if name == "controlled") {
                    break;
                }
                items.push(item);
            }
            // The slow tool is now blocked on release. The fast tool's result
            // must already be surfaceable — rig only flushes its buffered
            // results once the whole batch settles.
            let early = loop {
                let item = tokio::time::timeout(TIMEOUT, stream.next())
                    .await
                    .expect("the fast tool's result never surfaced while the slow tool runs")
                    .expect("stream ended before the fast result surfaced");
                let fast =
                    matches!(&item, StreamItem::ToolResult { name, .. } if name == "instant");
                items.push(item);
                if fast {
                    break true;
                }
            };
            release.notify_one();
            loop {
                let Some(item) = tokio::time::timeout(TIMEOUT, stream.next())
                    .await
                    .expect("stream stalled before the batch settled")
                else {
                    break;
                };
                items.push(item);
            }

            assert!(
                early,
                "the fast tool's result must surface while the slow tool still runs"
            );
            assert_eq!(
                results_named(&items, "instant"),
                1,
                "the buffered duplicate must be dropped"
            );
            assert_eq!(all_results(&items), 2);
            assert!(items.iter().any(
                |item| matches!(item, StreamItem::ToolResult { name, output, ok: true, .. }
                    if name == "controlled" && output == "controlled done")
            ));
            assert!(
                items
                    .iter()
                    .any(|item| matches!(item, StreamItem::Done { text, .. } if text == "done"))
            );
        }

        #[tokio::test]
        async fn worker_named_tool_surfaces_as_worker_result() {
            let release = std::sync::Arc::new(tokio::sync::Notify::new());
            let tools = vec![
                into_dynamic("instant", InstantTool),
                into_dynamic("edit_files", ControlledTool::new(release.clone())),
            ];
            let mut stream = agent_stream(
                batch_model("edit_files"),
                tools,
                HashSet::from(["edit_files".to_string()]),
            )
            .await;

            let mut items = Vec::new();
            loop {
                let item = tokio::time::timeout(TIMEOUT, stream.next())
                    .await
                    .expect("stream stalled before the worker tool started")
                    .expect("stream ended before the worker tool started");
                if matches!(&item, StreamItem::WorkerStart { name, .. } if name == "edit_files") {
                    break;
                }
                items.push(item);
            }
            // The worker tool is blocked on release; the fast tool's result
            // must already be surfaceable.
            loop {
                let item = tokio::time::timeout(TIMEOUT, stream.next())
                    .await
                    .expect("the fast tool's result never surfaced while the worker tool runs")
                    .expect("stream ended before the fast result surfaced");
                let fast =
                    matches!(&item, StreamItem::ToolResult { name, .. } if name == "instant");
                items.push(item);
                if fast {
                    break;
                }
            }
            release.notify_one();
            loop {
                let Some(item) = tokio::time::timeout(TIMEOUT, stream.next())
                    .await
                    .expect("stream stalled while the batch settles")
                else {
                    break;
                };
                items.push(item);
            }

            let worker_results: Vec<_> = items
                .iter()
                .filter(
                    |item| matches!(item, StreamItem::WorkerResult { name, .. } if name == "edit_files"),
                )
                .collect();
            assert_eq!(worker_results.len(), 1, "exactly one early worker result");
            assert!(matches!(
                worker_results[0],
                StreamItem::WorkerResult {
                    output,
                    ok: true,
                    ..
                } if output == "controlled done"
            ));
            assert_eq!(results_named(&items, "edit_files"), 0);
            assert_eq!(results_named(&items, "instant"), 1);
            assert!(
                items
                    .iter()
                    .any(|item| matches!(item, StreamItem::Done { text, .. } if text == "done"))
            );
        }

        #[tokio::test]
        async fn worker_internal_tool_results_surface_early() {
            let release = std::sync::Arc::new(tokio::sync::Notify::new());
            let tools = vec![
                into_dynamic("instant", InstantTool),
                into_dynamic("controlled", ControlledTool::new(release.clone())),
            ];
            let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel(64);
            let file_hook = FileChangeHook::new().with_early_finish(
                HashSet::new(),
                Some("run_tests".to_string()),
                activity_tx.clone(),
            );
            let agent = AgentBuilder::new(batch_model("controlled"))
                .dynamic_tools(tools)
                .add_hook(file_hook.clone())
                .build();
            let usage = std::sync::Arc::new(std::sync::Mutex::new(crate::TokenUsage::default()));
            let runner = tokio::spawn(run_worker_agent(
                agent,
                "run_tests",
                rig_core::message::Message::user("task"),
                activity_tx,
                usage,
                2,
                file_hook,
            ));

            // The worker's activity is a single FIFO channel: the batch's
            // start items surface before any tool runs, so break once the
            // slow tool starts, then observe the fast tool's result while
            // the slow tool is still blocked.
            let mut items = Vec::new();
            loop {
                let item = tokio::time::timeout(TIMEOUT, activity_rx.recv())
                    .await
                    .expect("stream stalled before the slow tool started")
                    .expect("activity channel closed before the slow tool finished");
                if matches!(&item, StreamItem::ToolStart { name, .. } if name == "controlled") {
                    break;
                }
                items.push(item);
            }
            loop {
                let item = tokio::time::timeout(TIMEOUT, activity_rx.recv())
                    .await
                    .expect("the fast tool's result never surfaced while the slow tool runs")
                    .expect("activity channel closed before the fast result surfaced");
                let fast =
                    matches!(&item, StreamItem::ToolResult { name, .. } if name == "instant");
                items.push(item);
                if fast {
                    break;
                }
            }
            let early = true;
            release.notify_one();
            loop {
                let Some(item) = tokio::time::timeout(TIMEOUT, activity_rx.recv())
                    .await
                    .expect("stream stalled while the batch settles")
                else {
                    break;
                };
                items.push(item);
            }
            let result = runner.await.unwrap().expect("worker run ok");

            assert_eq!(result, "done");
            assert!(
                early,
                "the fast tool's result must surface while the slow tool still runs"
            );
            assert_eq!(
                results_named(&items, "instant"),
                1,
                "the buffered duplicate must be dropped"
            );
            assert_eq!(results_named(&items, "controlled"), 1);
            for name in ["instant", "controlled"] {
                assert!(items.iter().any(
                    |item| matches!(item, StreamItem::ToolResult { name: n, worker: Some(w), .. }
                        if n == name && w == "run_tests")
                ));
            }
        }
    }
}
