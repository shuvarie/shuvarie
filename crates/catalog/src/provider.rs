use serde::{Deserialize, Serialize};

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
