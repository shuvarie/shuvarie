use futures_util::StreamExt;
use selune::{Dialect, ProviderType};
use std::sync::Arc;

use rig_core::driver::DynModel;
use rig_core::message::CallId;
use rig_core::operation::{Completion, Embedding};
use rig_core::providers::{anthropic, chatgpt, cohere, copilot, gemini, ollama, openai, voyageai};

use crate::auth::DeviceCodeHandler;
use crate::message::ChatMsg;
use crate::stream::{StreamItem, StreamStream};
use crate::tool::{DynamicTool, FileChangeHook};
use crate::{LlmError, Result};

/// The transport a provider connection streams on: the protocol kind plus the
/// optional vendor dialect. Selune 0.4 folds the OpenAI-compatible vendors
/// (`deepseek`, `groq`, `mistral`, …) under `ProviderType::OpenaiCompat`, with
/// `dialect` selecting the vendor flavor; a missing dialect is the generic
/// OpenAI-compatible surface. Config `kind` strings stay one-level: every
/// dialect name parses as this kind too (see the catalog's
/// `parse_provider_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderKind {
    pub r#type: ProviderType,
    pub dialect: Option<Dialect>,
}

impl ProviderKind {
    pub const fn new(r#type: ProviderType, dialect: Option<Dialect>) -> Self {
        Self { r#type, dialect }
    }
}

/// The rig wire dialect for a Selune vendor dialect: identical vendor names,
/// one-to-one (xAI's dialect lives in its own module). Vendor kinds that are
/// not `openai-compat` variants never reach here.
fn wire_dialect(dialect: Dialect) -> &'static openai::wire::Dialect {
    match dialect {
        Dialect::Deepseek => &openai::wire::DEEPSEEK,
        Dialect::Doubleword => &openai::wire::DOUBLEWORD,
        Dialect::Groq => &openai::wire::GROQ,
        Dialect::Huggingface => &openai::wire::HUGGINGFACE,
        Dialect::Hyperbolic => &openai::wire::HYPERBOLIC,
        Dialect::Minimax => &openai::wire::MINIMAX,
        Dialect::Mira => &openai::wire::MIRA,
        Dialect::Mistral => &openai::wire::MISTRAL,
        Dialect::Moonshot => &openai::wire::MOONSHOT,
        Dialect::Perplexity => &openai::wire::PERPLEXITY,
        Dialect::Together => &openai::wire::TOGETHER,
        Dialect::Venice => &openai::wire::VENICE,
        Dialect::Xai => &rig_core::providers::xai::DIALECT,
        Dialect::Xiaomimimo => &openai::wire::XIAOMIMIMO,
        Dialect::Zai => &openai::wire::ZAI,
    }
}

/// Whether a kind's surface serves embeddings. Protocol capabilities are the
/// transport's own; the `openai-compat` vendor dialects vary, with only the
/// vendors that actually wire `/embeddings` counted (the dialect only matters
/// on `openai-compat`, where it rides).
fn has_embeddings(kind: ProviderKind) -> bool {
    match (kind.r#type, kind.dialect) {
        (
            ProviderType::OpenaiCompat,
            Some(Dialect::Doubleword | Dialect::Mistral | Dialect::Together | Dialect::Venice),
        ) => true,
        (ProviderType::OpenaiCompat, Some(_)) => false,
        (
            ProviderType::Openai
            | ProviderType::OpenaiCompat
            | ProviderType::Openrouter
            | ProviderType::Google
            | ProviderType::Ollama
            | ProviderType::Vercel
            | ProviderType::Copilot
            | ProviderType::Azure
            | ProviderType::Cohere
            | ProviderType::Llamafile
            | ProviderType::Voyageai,
            _,
        ) => true,
        _ => false,
    }
}

/// Whether a kind's surface serves a model listing. As with embeddings, the
/// listed `openai-compat` vendor dialects wire a `/models` endpoint, and the
/// dialect only matters where it rides.
fn has_model_listing(kind: ProviderKind) -> bool {
    match (kind.r#type, kind.dialect) {
        (
            ProviderType::OpenaiCompat,
            Some(
                Dialect::Deepseek
                | Dialect::Groq
                | Dialect::Minimax
                | Dialect::Mira
                | Dialect::Mistral
                | Dialect::Moonshot
                | Dialect::Venice
                | Dialect::Xiaomimimo,
            ),
        ) => true,
        (ProviderType::OpenaiCompat, Some(_)) => false,
        (
            ProviderType::Openai
            | ProviderType::OpenaiCompat
            | ProviderType::Openrouter
            | ProviderType::Vercel
            | ProviderType::Google
            | ProviderType::Ollama
            | ProviderType::Copilot,
            _,
        ) => true,
        _ => false,
    }
}

#[derive(Debug, Clone)]
pub struct ProviderClient {
    kind: ProviderKind,
    base_url: Option<String>,
    list: ListImpl,
    /// One-time sign-in gate for the OAuth-backed transports (`chatgpt`,
    /// `copilot` without a credential): `list` is [`ListImpl::Pending`] until
    /// the first use resolves the signed-in client into the gate.
    oauth: Option<OAuthGate>,
}

/// The transports this client dispatches onto, by client type. OpenAI-shaped
/// providers (including every OpenAI-compatible vendor) share one client
/// type; the dialect inside its configuration picks the endpoints.
#[derive(Debug, Clone)]
enum ListImpl {
    OpenAi(Box<openai::OpenAI>),
    Anthropic(anthropic::Anthropic),
    Gemini(gemini::Gemini),
    Ollama(ollama::Ollama),
    Copilot(copilot::Copilot),
    Cohere(cohere::Cohere),
    Voyageai(voyageai::VoyageAi),
    /// Awaiting first-use sign-in; the gate builds and authenticates the
    /// client, and every operation reads the resolved transport from there.
    Pending,
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

/// An OpenAI-shaped client for `dialect` with `key`, on the shared reqwest
/// transport, honoring the endpoint override.
fn openai_client(
    dialect: &openai::wire::Dialect,
    key: &str,
    base_url: Option<&str>,
) -> openai::OpenAI {
    openai_client_with_route(dialect, key, base_url, None)
}

/// Like [`openai_client`], forcing the completion route when one is given.
/// A generic OpenAI-compatible server (no vendor dialect) serves Chat
/// Completions, not OpenAI's Responses endpoint.
fn openai_client_with_route(
    dialect: &openai::wire::Dialect,
    key: &str,
    base_url: Option<&str>,
    route: Option<openai::Route>,
) -> openai::OpenAI {
    let mut config = openai::OpenAIConfig::with_key(dialect, key);
    if let Some(url) = base_url {
        config = config.with_base_url(url);
    }
    if let Some(route) = route {
        config = config.with_route(route);
    }
    config.client()
}

/// GitHub token formats (`gho_`, `ghp_`, `ghu_`, `ghs_`, `ghr_`, fine-grained
/// `github_pat_`). A pasted Copilot credential that looks like one is a GitHub
/// token to exchange for a Copilot API key, not a Copilot API key itself.
fn is_github_token(token: &str) -> bool {
    ["gho_", "ghp_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| token.starts_with(prefix))
}

/// The ChatGPT subscription backend has no model-listing endpoint; these
/// known models stand in (the caller enriches them with catalog metadata by
/// id).
fn chatgpt_builtin_models() -> Vec<crate::Model> {
    // ChatGPT's Codex surface has no model-listing endpoint; these are the
    // models OpenAI recommends for subscription sign-in per the official
    // Codex models page.
    [
        ("gpt-6-astra", "GPT-6 Astra"),
        ("gpt-5.6-sol", "GPT-5.6 Sol"),
        ("gpt-5.6-terra", "GPT-5.6 Terra"),
        ("gpt-5.6-luna", "GPT-5.6 Luna"),
    ]
    .into_iter()
    .map(|(id, name)| rig_core::model::ModelInfo::new(id, name))
    .collect()
}

/// An OAuth-backed transport awaiting its first-use sign-in: the resolved
/// configuration plus the authenticator that builds the signed-in client.
#[derive(Debug, Clone)]
enum Pending {
    ChatGpt {
        config: Box<openai::OpenAIConfig>,
        authenticator: chatgpt::auth::Authenticator,
    },
    Copilot {
        config: copilot::CopilotConfig,
        authenticator: copilot::auth::Authenticator,
    },
}

impl Pending {
    async fn resolve(&self) -> Result<ListImpl> {
        match self {
            Pending::ChatGpt {
                config,
                authenticator,
            } => {
                let client = (**config)
                    .clone()
                    .client()
                    .authenticate(authenticator)
                    .await
                    .map_err(|error| LlmError::Provider(error.to_string()))?;
                Ok(ListImpl::OpenAi(Box::new(client)))
            }
            Pending::Copilot {
                config,
                authenticator,
            } => {
                let client = config
                    .clone()
                    .client()
                    .authenticate(authenticator)
                    .await
                    .map_err(|error| LlmError::Provider(error.to_string()))?;
                Ok(ListImpl::Copilot(client))
            }
        }
    }
}

/// One-time OAuth sign-in gate. Resolution reuses a cached credential when
/// present and valid, refreshes an expired one, and otherwise runs the
/// interactive device flow — firing the device-code callback supplied at
/// build time, which surfaces the verification URL + user code to the user.
/// A resolved transport is shared by every concurrent caller (clones share
/// the gate), so concurrent sign-ins serialize into one flow.
#[derive(Debug, Clone)]
struct OAuthGate {
    pending: Pending,
    resolved: tokio::sync::OnceCell<ListImpl>,
}

impl OAuthGate {
    fn new(pending: Pending) -> Self {
        Self {
            pending,
            resolved: tokio::sync::OnceCell::new(),
        }
    }

    async fn resolved(&self) -> Result<&ListImpl> {
        self.resolved
            .get_or_try_init(|| async { self.pending.resolve().await })
            .await
    }
}

/// The device-code callback wiring rig needs: shuvarie's handler wrapped in
/// rig's `DeviceCodeHandler`, and whether the interactive flow is allowed at
/// all (without a handler the sign-in fails fast with an actionable error
/// instead of blocking on an unattended flow).
fn device_code(handler: Option<&DeviceCodeHandler>) -> (chatgpt::auth::DeviceCodeHandler, bool) {
    match handler {
        Some(handler) => {
            let handler = Arc::clone(handler);
            (
                chatgpt::auth::DeviceCodeHandler::new(move |prompt| {
                    handler(crate::auth::DeviceCodePrompt {
                        verification_uri: prompt.verification_uri,
                        user_code: prompt.user_code,
                    });
                }),
                true,
            )
        }
        None => (chatgpt::auth::DeviceCodeHandler::default(), false),
    }
}

fn chatgpt_authenticator(handler: Option<&DeviceCodeHandler>) -> chatgpt::auth::Authenticator {
    let (device, allow_flow) = device_code(handler);
    chatgpt::auth::Authenticator::new(chatgpt::auth::AuthSource::OAuth, None, device, allow_flow)
}

fn copilot_authenticator(
    source: copilot::auth::AuthSource,
    handler: Option<&DeviceCodeHandler>,
) -> copilot::auth::Authenticator {
    let (device, allow_flow) = device_code(handler);
    copilot::auth::Authenticator::new(source, None, None, device, allow_flow)
}

impl ProviderClient {
    /// The selune type of this client's transport — the capability basis for
    /// image attachments (see `shuvarie_llm::Blobs` and the send path).
    pub const fn provider_type(&self) -> selune::ProviderType {
        self.kind.r#type
    }

    pub fn build(
        kind: ProviderKind,
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
        kind: ProviderKind,
        api_key: Option<&str>,
        base_url: Option<&str>,
        on_device_code: Option<DeviceCodeHandler>,
    ) -> Result<Self> {
        let base_url = base_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);

        // Build the OpenAi arm for one of the OpenAI-shaped vendor dialects:
        // the key is required, the endpoint override optional, and the
        // dialect supplies provider-specific defaults and request quirks.
        macro_rules! openai_vendor_client {
            ($dialect:expr) => {{
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                (
                    ListImpl::OpenAi(Box::new(openai_client(&$dialect, key, base_url.as_deref()))),
                    None,
                )
            }};
        }

        let (list, oauth) = match kind.r#type {
            ProviderType::Openai | ProviderType::Vercel => {
                openai_vendor_client!(openai::wire::OPENAI)
            }
            ProviderType::OpenaiCompat => {
                // The vendor dialect selects the wire (endpoints, quirks); a
                // missing dialect is the generic OpenAI-compatible surface,
                // which serves Chat Completions rather than the OpenAI
                // dialect's Responses endpoint.
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let route = kind.dialect.is_none().then_some(openai::Route::Chat);
                let dialect = kind
                    .dialect
                    .map(wire_dialect)
                    .unwrap_or(&openai::wire::OPENAI);
                (
                    ListImpl::OpenAi(Box::new(openai_client_with_route(
                        dialect,
                        key,
                        base_url.as_deref(),
                        route,
                    ))),
                    None,
                )
            }
            ProviderType::Openrouter => openai_vendor_client!(openai::wire::OPENROUTER),
            ProviderType::Anthropic => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let mut config = anthropic::AnthropicConfig::new(key);
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                (ListImpl::Anthropic(config.client()), None)
            }
            ProviderType::Google => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let mut config = gemini::GeminiConfig::new(key);
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                (ListImpl::Gemini(config.client()), None)
            }
            ProviderType::Ollama => {
                let mut config = ollama::OllamaConfig::new().with_api_key(api_key.unwrap_or(""));
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                (ListImpl::Ollama(config.client()), None)
            }
            ProviderType::Bedrock | ProviderType::GoogleVertex => {
                // These providers have no rig client (rig does not ship
                // bedrock or vertexai transports); fall back to an
                // OpenAI-compatible client at the configured URL.
                openai_vendor_client!(openai::wire::OPENAI)
            }
            ProviderType::Chatgpt => {
                let token = api_key.map(str::trim).filter(|k| !k.is_empty());
                // rig merges a generic assistant preamble into every request;
                // shuvarie always supplies its own preamble, so drop it and
                // tag the backend's telemetry with this app.
                let mut config =
                    openai::OpenAIConfig::with_key(&chatgpt::DIALECT, token.unwrap_or(""));
                config = config.with_instructions("");
                if let Some(identity) = config.identity.as_mut() {
                    identity.originator = "shuvarie".to_owned();
                }
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                match token {
                    Some(_) => (ListImpl::OpenAi(Box::new(config.client())), None),
                    None => {
                        let gate = OAuthGate::new(Pending::ChatGpt {
                            config: Box::new(config),
                            authenticator: chatgpt_authenticator(on_device_code.as_ref()),
                        });
                        (ListImpl::Pending, Some(gate))
                    }
                }
            }
            ProviderType::Copilot => {
                let token = api_key.map(str::trim).filter(|k| !k.is_empty());
                let mut config = copilot::CopilotConfig::new(token.unwrap_or(""));
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                match token {
                    // A pasted GitHub token exchanges for a Copilot session at
                    // first use; a plain Copilot key is the session itself.
                    Some(token) if !is_github_token(token) => {
                        (ListImpl::Copilot(config.client()), None)
                    }
                    Some(token) => {
                        let authenticator = copilot_authenticator(
                            copilot::auth::AuthSource::GitHubAccessToken(token.to_owned()),
                            on_device_code.as_ref(),
                        );
                        (
                            ListImpl::Pending,
                            Some(OAuthGate::new(Pending::Copilot {
                                config,
                                authenticator,
                            })),
                        )
                    }
                    None => {
                        let authenticator = copilot_authenticator(
                            copilot::auth::AuthSource::OAuth,
                            on_device_code.as_ref(),
                        );
                        (
                            ListImpl::Pending,
                            Some(OAuthGate::new(Pending::Copilot {
                                config,
                                authenticator,
                            })),
                        )
                    }
                }
            }
            ProviderType::Azure => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                // rig's Azure dialect carries no default endpoint: the
                // resource endpoint (https://{name}.openai.azure.com) is
                // required on the connection.
                let endpoint = base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                    .ok_or(LlmError::Provider("Azure endpoint required".into()))?;
                (
                    ListImpl::OpenAi(Box::new(
                        openai::OpenAIConfig::with_key(&openai::wire::AZURE, key)
                            .with_base_url(endpoint)
                            .client(),
                    )),
                    None,
                )
            }
            ProviderType::Llamafile => {
                // llama.cpp's `--api-key` is optional: an empty credential
                // sends no Authorization header at all.
                openai_vendor_client!(openai::wire::LLAMACPP)
            }
            ProviderType::Cohere => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let mut config = cohere::CohereConfig::new(key);
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                (ListImpl::Cohere(config.client()), None)
            }
            ProviderType::Voyageai => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let mut config = voyageai::VoyageAiConfig::new(key);
                if let Some(url) = base_url.as_deref() {
                    config = config.with_base_url(url);
                }
                (ListImpl::Voyageai(config.client()), None)
            }
        };
        Ok(Self {
            kind,
            base_url,
            list,
            oauth,
        })
    }

    /// The transport to operate on, resolving one-time OAuth sign-in when an
    /// OAuth-backed transport is pending (`chatgpt`, `copilot` without a
    /// credential). A cached credential opens without network, an expired one
    /// refreshes, and a missing one runs the interactive device flow through
    /// the device-code callback supplied at build time.
    async fn resolved_list(&self) -> Result<&ListImpl> {
        match &self.oauth {
            Some(gate) => gate.resolved().await,
            None => Ok(&self.list),
        }
    }

    /// Run OAuth sign-in to completion for an OAuth-backed provider (`chatgpt`,
    /// `copilot`). Reuses a cached credential when present and valid, refreshes
    /// an expired one, and otherwise runs the interactive device flow — firing
    /// the device-code callback supplied at build time, which surfaces the
    /// verification URL + user code to the user. Resolves once the client holds
    /// a usable token, so it can be awaited off the update loop. Providers
    /// without OAuth sign-in return an error instead of blocking.
    pub async fn authorize(&self) -> Result<()> {
        if let Some(gate) = &self.oauth {
            return gate.resolved().await.map(|_| ());
        }
        match self.kind.r#type {
            // Already usable: the credential is on the client itself.
            ProviderType::Chatgpt | ProviderType::Copilot => Ok(()),
            _ => Err(LlmError::Provider(format!(
                "{} does not use OAuth sign-in; configure an API key instead",
                self.kind_name()
            ))),
        }
    }

    /// Lowercase kebab-case transport name for user-facing messages.
    fn kind_name(&self) -> &'static str {
        match self.kind.r#type {
            ProviderType::Chatgpt => "chatgpt",
            ProviderType::Copilot => "copilot",
            _ => "this provider",
        }
    }

    pub fn kind(&self) -> ProviderKind {
        self.kind
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn supports_embeddings(&self) -> bool {
        has_embeddings(self.kind)
    }

    pub async fn embed(&self, model: &str, dims: usize, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if !self.supports_embeddings() {
            return Err(LlmError::Embedding(
                "provider does not support embeddings".into(),
            ));
        }
        let list = self.resolved_list().await?;
        // Ollama's embedding width is chosen per request; every other
        // provider sizes from the model.
        let ndims = if matches!(self.kind.r#type, ProviderType::Ollama) {
            Some(dims)
        } else {
            None
        };
        let Some(model) = list.embedding_model(model, ndims) else {
            return Err(LlmError::Embedding(
                "provider does not support embeddings".into(),
            ));
        };
        embed_via(model, texts.to_vec()).await
    }

    pub async fn list_models(&self) -> Result<Vec<crate::Model>> {
        if matches!(self.kind.r#type, ProviderType::Chatgpt) {
            return Ok(chatgpt_builtin_models());
        }
        if !has_model_listing(self.kind) {
            return Err(LlmError::Model(
                "provider does not support model listing".into(),
            ));
        }
        let list = self.resolved_list().await?;
        let models = list.list_models().await?;
        Ok(models.data)
    }

    pub async fn run_worker(
        &self,
        req: &crate::agent::WorkerRequest,
    ) -> std::result::Result<String, String> {
        let list = self.resolved_list().await.map_err(|e| e.to_string())?;
        let Some(model) = list.completion_model(&req.model) else {
            return Err("voyageai supports embeddings only, not completions".to_string());
        };
        worker_via(model, req).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stream(
        &self,
        model: &str,
        preamble: Option<&str>,
        prompt: &str,
        history: &[ChatMsg],
        // Attachments of the prompt message itself (the newest user turn).
        prompt_attachments: &[crate::attachment::Attachment],
        // Content bytes for every referenced attachment's sha256 — both the
        // prompt's and history's (see `shuvarie_llm::Blobs`).
        blobs: &crate::attachment::Blobs,
        tools: Vec<DynamicTool>,
        workers: &mut [crate::agent::WorkerAgent],
        max_turns: usize,
        tool_concurrency: usize,
        context_budget: Option<crate::context_hook::ContextBudget>,
        seed_usage: Option<crate::TokenUsage>,
    ) -> StreamStream {
        let list = match self.resolved_list().await {
            Ok(list) => list,
            Err(error) => {
                return Box::pin(futures_util::stream::iter([StreamItem::Error {
                    message: error.to_string(),
                    reason: "Sign-in required".into(),
                }]));
            }
        };
        let Some(model) = list.completion_model(model) else {
            return Box::pin(futures_util::stream::iter([StreamItem::Error {
                message: "voyageai supports embeddings only, not completions".into(),
                reason: "Turn error".into(),
            }]));
        };
        let tracker = crate::context_hook::UsageTracker::new();
        // Seed the measured compaction trigger with the previous turn's last
        // main request, so the first call of this run is anchored on real
        // usage instead of a chars/4 estimate. A fresh session has no seed
        // and stays estimate-only until its first call reports usage; a
        // zero-usage seed is ignored, and one larger than the model's window
        // (a row written before the per-request payload, holding the whole
        // run's aggregate) is dropped rather than arming the stop against a
        // conversation that never existed.
        if let (Some(seed), Some(budget)) = (seed_usage, context_budget.as_ref()) {
            tracker.seed(seed, budget.context_length);
        }
        let tracker_for_hook = tracker.clone();
        let mut prompt_msg = ChatMsg::user(prompt.to_string());
        prompt_msg.attachments = prompt_attachments.to_vec();
        let user_msg = crate::message::to_rig_message(prompt_msg, blobs);
        let rig_history: Vec<rig_core::message::Message> = history
            .iter()
            .cloned()
            .map(|msg| crate::message::to_rig_message(msg, blobs))
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
            FileChangeHook::new().with_early_finish(worker_names.clone(), None, None, early_tx);
        receivers.push(early_rx);

        stream_via(
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
            tool_concurrency,
            tracker,
        )
        .await
    }
}

impl ListImpl {
    /// The completion model `id` addresses, erased to the operation the agent
    /// runtime consumes. Each dialect's default completion route applies: the
    /// Responses endpoint for OpenAI and the ChatGPT subscription backend,
    /// chat completions for the OpenAI-compatible vendors and Azure's
    /// deployment routing.
    fn completion_model(&self, id: &str) -> Option<DynModel<Completion>> {
        match self {
            ListImpl::OpenAi(c) => Some(c.completion(id).erase()),
            ListImpl::Anthropic(c) => Some(c.completion(id).erase()),
            ListImpl::Gemini(c) => Some(c.completion(id).erase()),
            ListImpl::Ollama(c) => Some(c.completion(id).erase()),
            ListImpl::Copilot(c) => Some(c.completion(id).erase()),
            ListImpl::Cohere(c) => Some(c.completion(id).erase()),
            ListImpl::Voyageai(_) | ListImpl::Pending => None,
        }
    }

    /// The embedding model `id` addresses, `ndims` wide when the provider
    /// sizes embeddings per request.
    fn embedding_model(&self, id: &str, ndims: Option<usize>) -> Option<DynModel<Embedding>> {
        match self {
            ListImpl::OpenAi(c) => Some(c.embedding(id, ndims).erase()),
            ListImpl::Gemini(c) => Some(c.embedding(id, ndims).erase()),
            ListImpl::Ollama(c) => Some(c.embedding(id, ndims).erase()),
            ListImpl::Copilot(c) => Some(c.embedding(id, ndims).erase()),
            ListImpl::Cohere(c) => Some(c.embedding(id, ndims).erase()),
            ListImpl::Voyageai(c) => Some(c.embedding(id, ndims).erase()),
            ListImpl::Anthropic(_) | ListImpl::Pending => None,
        }
    }

    /// The models this transport serves, every page followed.
    async fn list_models(&self) -> std::result::Result<rig_core::model::ModelList, LlmError> {
        match self {
            ListImpl::OpenAi(c) => c
                .list_models()
                .await
                .map_err(|e| LlmError::Model(e.to_string())),
            ListImpl::Anthropic(c) => c
                .list_models()
                .await
                .map_err(|e| LlmError::Model(e.to_string())),
            ListImpl::Gemini(c) => c
                .list_models()
                .await
                .map_err(|e| LlmError::Model(e.to_string())),
            ListImpl::Ollama(c) => c
                .list_models()
                .await
                .map_err(|e| LlmError::Model(e.to_string())),
            ListImpl::Copilot(c) => c
                .list_models()
                .await
                .map_err(|e| LlmError::Model(e.to_string())),
            ListImpl::Cohere(_) | ListImpl::Voyageai(_) | ListImpl::Pending => Err(
                LlmError::Model("provider does not support model listing".into()),
            ),
        }
    }
}

/// Embed `texts` through a rig embedding model, converting to `f32` vectors.
async fn embed_via(model: DynModel<Embedding>, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    use rig_core::embeddings::EmbeddingsBuilder;

    let embeddings = EmbeddingsBuilder::new(model)
        .documents(texts.to_vec())
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

/// Run a worker-agent request against an erased completion model.
async fn worker_via(
    model: DynModel<Completion>,
    req: &crate::agent::WorkerRequest,
) -> std::result::Result<String, String> {
    let user_msg = rig_core::message::Message::user(req.task.clone());
    let activity_tx = req.activity_tx.clone();
    let usage = std::sync::Arc::clone(&req.usage);
    let tracker = crate::context_hook::UsageTracker::new();
    let file_hook = FileChangeHook::new().with_early_finish(
        std::collections::HashSet::new(),
        Some(req.name.clone()),
        Some(req.spawn),
        activity_tx.clone(),
    );
    let agent = agent_with_tools(
        model,
        Some(&req.preamble),
        req.tools.clone(),
        req.context_budget.clone(),
        tracker,
        file_hook.clone(),
    );
    run_worker_agent(
        agent,
        &req.name,
        req.spawn,
        user_msg,
        activity_tx,
        usage,
        req.max_turns,
        file_hook,
    )
    .await
}

/// Stream a chat turn through an erased completion model.
#[allow(clippy::too_many_arguments)]
async fn stream_via(
    model: DynModel<Completion>,
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
    tool_concurrency: usize,
    tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
) -> StreamStream {
    let agent = agent_with_tools(
        model,
        preamble,
        dynamic,
        context_budget,
        tracker_for_hook,
        file_hook.clone(),
    );
    // `tool_concurrency` caps how many tools run at once within an assistant
    // message — a batch of worker briefs in an orchestration scene spawns in
    // parallel; sequential (`1`) stays the unset default. Streamed output
    // ordering is preserved either way.
    let stream = agent
        .prompt(user_msg)
        .history(rig_history)
        .max_turns(max_turns)
        .tool_concurrency(tool_concurrency)
        .stream();
    map_agent_stream(stream, receivers, worker_names, tracker, file_hook)
}

#[allow(clippy::too_many_arguments)]
fn agent_with_tools(
    model: DynModel<Completion>,
    preamble: Option<&str>,
    dynamic: Vec<DynamicTool>,
    context_budget: Option<crate::context_hook::ContextBudget>,
    tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
    file_hook: FileChangeHook,
) -> rig_agent::agent::Agent {
    let builder = rig_agent::agent::AgentBuilder::new(model);
    let builder = match preamble {
        Some(p) => builder.preamble(p),
        None => builder.without_preamble(),
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

#[allow(clippy::too_many_arguments)]
async fn run_worker_agent(
    agent: rig_agent::agent::Agent,
    name: &str,
    spawn: u64,
    prompt: rig_core::message::Message,
    activity_tx: tokio::sync::mpsc::Sender<StreamItem>,
    usage: std::sync::Arc<std::sync::Mutex<crate::TokenUsage>>,
    max_turns: usize,
    file_hook: FileChangeHook,
) -> std::result::Result<String, String> {
    let mut stream = agent.prompt(prompt).max_turns(max_turns).stream();

    let mut turn_text = TurnText::default();
    let mut usage_aggregate = crate::TokenUsage::default();
    let mut tool_names: std::collections::HashMap<CallId, String> =
        std::collections::HashMap::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
                rig_core::streaming::Item::Event(rig_core::streaming::StreamEvent::Text {
                    text,
                    ..
                }),
            )) => {
                turn_text.push(text);
            }
            Ok(rig_agent::agent::MultiTurnStreamItem::ToolCall { tool_call }) => {
                tool_names.insert(tool_call.id.clone(), tool_call.function.name.to_string());
                let _ = activity_tx
                    .send(StreamItem::ToolStart {
                        name: tool_call.function.name.to_string(),
                        args: tool_call.function.arguments,
                        worker: Some(name.to_string()),
                        spawn: Some(spawn),
                        call_id: tool_call.id.to_string(),
                    })
                    .await;
            }
            Ok(rig_agent::agent::MultiTurnStreamItem::StreamUserItem(
                rig_core::streaming::StreamedUserContent::ToolResult { tool_result },
            )) if !file_hook.surfaced_early(&tool_result.call) => {
                let call_id = tool_result.call.clone();
                let tool_name = tool_names.remove(&call_id).unwrap_or_default();
                let captured = file_hook.take(&call_id);
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
                        spawn: Some(spawn),
                        file_change: captured.file_change,
                        streams: captured.shell,
                        call_id: call_id.to_string(),
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
        *guard += usage_aggregate;
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
    let mut tool_names: std::collections::HashMap<CallId, String> =
        std::collections::HashMap::new();
    let mut pending_workers: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let tracker_clone = tracker.clone();
    let main = stream.map(move |item| match item {
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
            rig_core::streaming::Item::Event(rig_core::streaming::StreamEvent::Text {
                text, ..
            }),
        )) => StreamItem::Delta {
            text: turn_text.push(text),
        },
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamAssistantItem(
            rig_core::streaming::Item::Event(rig_core::streaming::StreamEvent::Reasoning {
                text,
                ..
            }),
        )) => {
            if text.is_empty() {
                StreamItem::Delta {
                    text: String::new(),
                }
            } else {
                StreamItem::Reasoning { text }
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::ToolCall { tool_call }) => {
            tool_called = true;
            turn_text.tool_called();
            let name = tool_call.function.name.to_string();
            let call_id = tool_call.id.clone();
            if worker_names.contains(&name) {
                pending_workers.push_back(name.clone());
                StreamItem::WorkerStart {
                    name,
                    args: tool_call.function.arguments,
                    call_id: call_id.to_string(),
                }
            } else {
                tool_names.insert(call_id.clone(), name.clone());
                StreamItem::ToolStart {
                    name,
                    args: tool_call.function.arguments,
                    worker: None,
                    spawn: None,
                    call_id: call_id.to_string(),
                }
            }
        }
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamUserItem(
            rig_core::streaming::StreamedUserContent::ToolResult { tool_result },
        )) if file_hook.surfaced_early(&tool_result.call) => StreamItem::Delta {
            text: String::new(),
        },
        Ok(rig_agent::agent::MultiTurnStreamItem::StreamUserItem(
            rig_core::streaming::StreamedUserContent::ToolResult { tool_result },
        )) => {
            let call_id = tool_result.call.clone();
            let mut output = String::new();
            for content in tool_result.content.iter() {
                if let Some(text) = content.as_text() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(text);
                }
            }
            let captured = file_hook.take(&call_id);
            let mut ok = !captured.failed;
            if output.is_empty() {
                ok = false;
                output = String::from("(no output)");
            }
            let name = tool_names.remove(&call_id);
            match name {
                Some(name) => StreamItem::ToolResult {
                    name,
                    output,
                    ok,
                    worker: None,
                    spawn: captured.spawn,
                    file_change: captured.file_change,
                    streams: captured.shell,
                    call_id: call_id.to_string(),
                },
                None => StreamItem::WorkerResult {
                    name: pending_workers.pop_front().unwrap_or_default(),
                    output,
                    ok,
                    spawn: captured.spawn,
                    call_id: call_id.to_string(),
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
            if message.contains(crate::context_hook::OVERFLOW_REASON)
                || crate::retry::is_context_length_error(&message)
            {
                // Either the hook stopped the run pre-call (the measured
                // trigger) or the provider itself rejected the request as
                // too long (a genuine overflow that slipped past the
                // trigger): both compact the session and auto-continue.
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

    /// A provider kind without a vendor dialect (the tests' common shape).
    fn kind(t: selune::ProviderType) -> ProviderKind {
        ProviderKind::new(t, None)
    }

    /// A provider kind riding one of Selune's openai-compat vendor dialects.
    fn vendor(d: selune::Dialect) -> ProviderKind {
        ProviderKind::new(selune::ProviderType::OpenaiCompat, Some(d))
    }

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
                spawn: Some(1),
                call_id: "w1".into(),
            },
            StreamItem::ToolResult {
                name: "read_file".into(),
                output: "ok".into(),
                ok: true,
                worker: Some("explore_workspace".into()),
                spawn: Some(1),
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
            spawn: Some(1),
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
        let client = ProviderClient::build(kind(selune::ProviderType::Ollama), None, None).unwrap();
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
        for p in [
            Openai,
            OpenaiCompat,
            Openrouter,
            Google,
            Ollama,
            Vercel,
            Copilot,
        ] {
            let client = ProviderClient::build(kind(p), Some("k"), None).unwrap();
            assert!(
                client.supports_embeddings(),
                "{p:?} should support embeddings"
            );
        }
        let no = [Anthropic, Bedrock, GoogleVertex, Chatgpt];
        for p in no {
            let client = ProviderClient::build(kind(p), Some("k"), None).unwrap();
            assert!(!client.supports_embeddings(), "{p:?} should not");
        }
        // The remaining embeddings-capable transports; Azure's resource
        // endpoint is required at build time.
        let azure =
            ProviderClient::build(kind(Azure), Some("k"), Some("https://res.openai.azure.com"))
                .unwrap();
        assert!(azure.supports_embeddings());
        for p in [Cohere, Llamafile, Voyageai] {
            let client = ProviderClient::build(kind(p), Some("k"), None).unwrap();
            assert!(
                client.supports_embeddings(),
                "{p:?} should support embeddings"
            );
        }
        // Openai-compat vendor dialects: every dialect builds, and the vendor
        // flavor decides whether embeddings exist — these wire `/embeddings`,
        // the rest do not.
        use selune::Dialect::*;
        for d in [
            Deepseek,
            Doubleword,
            Groq,
            Huggingface,
            Hyperbolic,
            Minimax,
            Mira,
            Mistral,
            Moonshot,
            Perplexity,
            Together,
            Venice,
            Xai,
            Xiaomimimo,
            Zai,
        ] {
            let supported = matches!(d, Doubleword | Mistral | Together | Venice);
            let client = ProviderClient::build(vendor(d), Some("k"), None).unwrap();
            assert_eq!(
                client.supports_embeddings(),
                supported,
                "{d:?} embedding support"
            );
        }
    }

    #[test]
    fn generic_openai_compat_serves_chat_completions_not_responses() {
        let client = ProviderClient::build(
            kind(selune::ProviderType::OpenaiCompat),
            Some("k"),
            Some("http://localhost:8080/v1"),
        )
        .unwrap();
        let ListImpl::OpenAi(openai) = &client.list else {
            panic!("openai-compat builds an OpenAI client");
        };
        assert_eq!(
            openai.config().completion_route(),
            openai::Route::Chat,
            "a dialect-less OpenAI-compatible server only serves Chat Completions"
        );

        // A vendor dialect keeps its own Chat route.
        let deepseek =
            ProviderClient::build(vendor(selune::Dialect::Deepseek), Some("k"), None).unwrap();
        let ListImpl::OpenAi(openai) = &deepseek.list else {
            panic!("a dialect builds an OpenAI client");
        };
        assert_eq!(openai.config().completion_route(), openai::Route::Chat);

        // Official OpenAI keeps the Responses route.
        let native =
            ProviderClient::build(kind(selune::ProviderType::Openai), Some("k"), None).unwrap();
        let ListImpl::OpenAi(openai) = &native.list else {
            panic!("openai builds an OpenAI client");
        };
        assert_eq!(openai.config().completion_route(), openai::Route::Responses);
    }

    #[test]
    fn auth_backed_clients_build_with_and_without_a_key() {
        use selune::ProviderType::*;
        for t in [Chatgpt, Copilot] {
            let keyed = ProviderClient::build(kind(t), Some("tok"), None).unwrap();
            assert_eq!(keyed.kind(), kind(t));
            let oauth = ProviderClient::build(kind(t), None, None).unwrap();
            assert_eq!(oauth.kind(), kind(t));
            let handler: crate::auth::DeviceCodeHandler = std::sync::Arc::new(|_| {});
            let prompted =
                ProviderClient::build_with_device_code(kind(t), None, None, Some(handler)).unwrap();
            assert_eq!(prompted.kind(), kind(t));
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
        // A pasted session/access token answers directly — no HTTP, no cache
        // files touched, no sign-in gate.
        let chatgpt = ProviderClient::build(kind(Chatgpt), Some("tok"), None).unwrap();
        chatgpt.authorize().await.expect("static token authorizes");
        let copilot = ProviderClient::build(kind(Copilot), Some("copilot-key"), None).unwrap();
        copilot
            .authorize()
            .await
            .expect("static api key authorizes");
    }

    #[tokio::test]
    async fn authorize_rejects_non_oauth_kinds() {
        let client =
            ProviderClient::build(kind(selune::ProviderType::Openai), Some("k"), None).unwrap();
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
        let client =
            ProviderClient::build(kind(selune::ProviderType::Chatgpt), None, None).unwrap();
        let models = client.list_models().await.unwrap();
        assert!(!models.is_empty());
    }

    #[tokio::test]
    async fn embed_unsupported_provider_errors() {
        let client =
            ProviderClient::build(kind(selune::ProviderType::Anthropic), Some("k"), None).unwrap();
        let err = client
            .embed("some-model", 768, &["hello".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Embedding(_)));
    }

    #[tokio::test]
    async fn embed_empty_input_returns_empty() {
        let client = ProviderClient::build(kind(selune::ProviderType::Ollama), None, None).unwrap();
        let out = client.embed("m", 384, &[]).await.unwrap();
        assert!(out.is_empty());
    }

    mod early_tool_results {
        use super::*;
        use crate::tool::{ToolContext, ToolExecutionError, ToolOutput, into_dynamic};
        use rig_agent::agent::AgentBuilder;
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
                FileChangeHook::new().with_early_finish(worker_names.clone(), None, None, early_tx);
            let agent = AgentBuilder::new(model)
                .dynamic_tools(tools)
                .add_hook(file_hook.clone())
                .build();
            let stream = agent
                .prompt(rig_core::message::Message::user("go"))
                .max_turns(2)
                .stream();
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
                Some(7),
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
                7,
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
