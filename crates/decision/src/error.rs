use thiserror::Error;

pub type Result<T> = std::result::Result<T, DecisionError>;

/// A decision request or definition failed.
#[derive(Debug, Error)]
pub enum DecisionError {
    /// A decision request failed at the provider or on the wire. The provider
    /// label is the configured connection id, not the protocol: the decision
    /// wire is shared by TypeSafe, Ollama, and Cloudflare, so the backend the
    /// user configured is the actionable half of the message.
    #[error("decision provider `{provider}`: {message}")]
    Provider { provider: String, message: String },

    /// A decision definition is outside the System One protocol (bounds,
    /// duplicates, empty labels) and was rejected before a request was built.
    #[error("invalid decision `{name}`: {message}")]
    Invalid { name: String, message: String },

    /// The evaluation state exceeds the System One request budget. The state
    /// is never truncated silently: a truncated command could hide the very
    /// text a safety judgement is about.
    #[error(
        "decision state is {size} bytes, over the {limit}-byte System One limit (without images)"
    )]
    StateTooLarge { size: usize, limit: usize },
}
