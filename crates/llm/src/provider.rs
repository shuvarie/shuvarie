use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::ChatMsg;
use crate::stream::{StreamItem, StreamStream};
use crate::tool::Tool;
use crate::usage::TokenUsage;
use crate::{LlmError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    OpenAiCompatible,
    OpenRouter,
    Groq,
    Together,
    DeepSeek,
    Anthropic,
    Gemini,
    Ollama,
    OllamaCloud,
}

impl Provider {
    pub const ALL: [Provider; 9] = [
        Provider::OpenAiCompatible,
        Provider::OpenRouter,
        Provider::Groq,
        Provider::Together,
        Provider::DeepSeek,
        Provider::Anthropic,
        Provider::Gemini,
        Provider::Ollama,
        Provider::OllamaCloud,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Provider::OpenAiCompatible => "OpenAI-compatible",
            Provider::OpenRouter => "OpenRouter",
            Provider::Groq => "Groq",
            Provider::Together => "Together",
            Provider::DeepSeek => "DeepSeek",
            Provider::Anthropic => "Anthropic",
            Provider::Gemini => "Gemini",
            Provider::Ollama => "Ollama",
            Provider::OllamaCloud => "Ollama Cloud",
        }
    }

    pub const fn requires_api_key(self) -> bool {
        !matches!(self, Provider::Ollama)
    }

    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Provider::OpenAiCompatible => Some("https://api.openai.com/v1"),
            Provider::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Provider::Groq => Some("https://api.groq.com/openai/v1"),
            Provider::Together => Some("https://api.together.xyz"),
            Provider::DeepSeek => Some("https://api.deepseek.com"),
            Provider::Anthropic => Some("https://api.anthropic.com"),
            Provider::Gemini => Some("https://generativelanguage.googleapis.com"),
            Provider::Ollama => Some("http://localhost:11434"),
            Provider::OllamaCloud => Some("https://ollama.com"),
        }
    }

    pub fn effective_base_url(self, override_url: Option<&str>) -> String {
        override_url
            .map(str::to_owned)
            .unwrap_or_else(|| self.default_base_url().unwrap_or("").to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct ProviderClient {
    kind: Provider,
    base_url: String,
    list: ListImpl,
}

#[derive(Debug, Clone)]
enum ListImpl {
    OpenAi(rig::providers::openai::Client),
    OpenRouter(rig::providers::openrouter::Client),
    DeepSeek(rig::providers::deepseek::Client),
    Anthropic(rig::providers::anthropic::Client),
    Gemini(rig::providers::gemini::Client),
    Ollama(rig::providers::ollama::Client),
}

impl ProviderClient {
    pub fn build(kind: Provider, api_key: Option<&str>, base_url: Option<&str>) -> Result<Self> {
        let base_url = kind.effective_base_url(base_url);
        let list = match kind {
            Provider::OpenAiCompatible => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::openai::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
            Provider::OpenRouter => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::openrouter::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenRouter(client)
            }
            Provider::Groq => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::openai::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
            Provider::Together => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::openai::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
            Provider::DeepSeek => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::deepseek::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::DeepSeek(client)
            }
            Provider::Anthropic => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::anthropic::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::Anthropic(client)
            }
            Provider::Gemini => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::gemini::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::Gemini(client)
            }
            Provider::Ollama => {
                let client = rig::providers::ollama::Client::builder()
                    .api_key(api_key.unwrap_or(""))
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::Ollama(client)
            }
            Provider::OllamaCloud => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::ollama::Client::builder()
                    .api_key(key)
                    .base_url(&base_url)
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::Ollama(client)
            }
        };
        Ok(Self {
            kind,
            base_url,
            list,
        })
    }

    pub fn kind(&self) -> Provider {
        self.kind
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn list_models(&self) -> Result<Vec<crate::model::ModelInfo>> {
        use rig::client::ModelListingClient;

        let models = match &self.list {
            ListImpl::OpenAi(c) => c.list_models().await,
            ListImpl::OpenRouter(c) => c.list_models().await,
            ListImpl::DeepSeek(c) => c.list_models().await,
            ListImpl::Anthropic(c) => c.list_models().await,
            ListImpl::Gemini(c) => c.list_models().await,
            ListImpl::Ollama(c) => c.list_models().await,
        }
        .map_err(|e| LlmError::Model(e.to_string()))?;

        Ok(models
            .iter()
            .map(crate::model::ModelInfo::from_rig)
            .collect())
    }

    pub async fn stream(
        &self,
        model: &str,
        preamble: Option<&str>,
        prompt: &str,
        history: &[ChatMsg],
        tools: &[std::sync::Arc<dyn Tool>],
    ) -> StreamStream {
        use futures_util::StreamExt;
        use rig::streaming::StreamingChat;

        let user_msg = rig::message::Message::user(prompt.to_string());
        let rig_history: Vec<rig::message::Message> = history
            .iter()
            .cloned()
            .map(rig::message::Message::from)
            .collect();
        let dynamic: Vec<rig::tool::DynamicTool> = tools
            .iter()
            .map(|tool| {
                let def = tool.definition();
                let tool = std::sync::Arc::clone(tool);
                rig::tool::DynamicTool::new(
                    def.name.clone(),
                    def.description.clone(),
                    def.parameters.clone(),
                    move |_ctx, args| {
                        let tool = std::sync::Arc::clone(&tool);
                        Box::pin(async move {
                            tool.call(args)
                                .await
                                .map(rig::tool::ToolOutput::text)
                                .map_err(|message| {
                                    let error = serde_json::json!({ "error": message });
                                    rig::tool::ToolExecutionError::new(
                                        rig::tool::ToolErrorKind::Other,
                                        message,
                                    )
                                    .with_model_output(rig::tool::ToolOutput::json(error))
                                })
                        })
                    },
                )
            })
            .collect();

        fn agent_with_tools<C>(
            client: &C,
            model: &str,
            preamble: Option<&str>,
            dynamic: Vec<rig::tool::DynamicTool>,
        ) -> rig::agent::Agent<C::CompletionModel>
        where
            C: rig::client::CompletionClient + rig::prelude::AgentClientExt,
        {
            let builder = match preamble {
                Some(p) => client.agent(model).preamble(p),
                None => client.agent(model).without_preamble(),
            };
            builder.dynamic_tools(dynamic).build()
        }

        async fn build<M>(
            agent: rig::agent::Agent<M>,
            prompt: rig::message::Message,
            history: Vec<rig::message::Message>,
        ) -> StreamStream
        where
            M: rig::completion::CompletionModel + 'static,
        {
            let stream = agent.stream_chat(prompt, history).max_turns(20).await;
            let mut tool_called = false;
            let mut accumulated = String::new();
            let mut tool_names: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            Box::pin(stream.map(move |item| match item {
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::Text(t),
                )) => {
                    accumulated.push_str(&t.text);
                    StreamItem::Delta { text: t.text }
                }
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id,
                    },
                )) => {
                    tool_called = true;
                    tool_names.insert(internal_call_id, tool_call.function.name.clone());
                    StreamItem::ToolStart {
                        name: tool_call.function.name,
                        args: tool_call.function.arguments,
                    }
                }
                Ok(rig::agent::MultiTurnStreamItem::StreamUserItem(
                    rig::streaming::StreamedUserContent::ToolResult {
                        tool_result,
                        internal_call_id,
                    },
                )) => {
                    let name = tool_names.remove(&internal_call_id).unwrap_or_default();
                    let mut output = String::new();
                    let mut ok = true;
                    for content in tool_result.content.iter() {
                        if let Some(text) = content.as_text() {
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(text);
                        }
                    }
                    if output.is_empty() {
                        ok = false;
                        output = String::from("(no output)");
                    } else if output.starts_with("{\"error\":") {
                        ok = false;
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&output)
                            && let Some(message) = value.get("error").and_then(Value::as_str)
                        {
                            output = message.to_string();
                        }
                    }
                    StreamItem::ToolResult { name, output, ok }
                }
                Ok(rig::agent::MultiTurnStreamItem::FinalResponse(resp)) => {
                    let text = if accumulated.is_empty() && tool_called {
                        String::new()
                    } else {
                        std::mem::take(&mut accumulated)
                    };
                    StreamItem::Done {
                        text,
                        usage: TokenUsage::from_rig(resp.usage),
                    }
                }
                Ok(_) => StreamItem::Delta {
                    text: String::new(),
                },
                Err(e) => StreamItem::Error {
                    message: e.to_string(),
                },
            }))
        }

        match &self.list {
            ListImpl::OpenAi(c) => {
                build(
                    agent_with_tools(c, model, preamble, dynamic),
                    user_msg,
                    rig_history,
                )
                .await
            }
            ListImpl::OpenRouter(c) => {
                build(
                    agent_with_tools(c, model, preamble, dynamic),
                    user_msg,
                    rig_history,
                )
                .await
            }
            ListImpl::DeepSeek(c) => {
                build(
                    agent_with_tools(c, model, preamble, dynamic),
                    user_msg,
                    rig_history,
                )
                .await
            }
            ListImpl::Anthropic(c) => {
                build(
                    agent_with_tools(c, model, preamble, dynamic),
                    user_msg,
                    rig_history,
                )
                .await
            }
            ListImpl::Gemini(c) => {
                build(
                    agent_with_tools(c, model, preamble, dynamic),
                    user_msg,
                    rig_history,
                )
                .await
            }
            ListImpl::Ollama(c) => {
                build(
                    agent_with_tools(c, model, preamble, dynamic),
                    user_msg,
                    rig_history,
                )
                .await
            }
        }
    }

    pub fn estimate_cost(&self, usage: &TokenUsage) -> f64 {
        crate::pricing::estimate_cost(self.kind, usage)
    }
}
