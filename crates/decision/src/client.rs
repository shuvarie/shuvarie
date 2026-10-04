//! How to ask: the System One connection and its request.

use std::collections::BTreeMap;

use rig_core::error::ProviderError;
use rig_typesafeai::{DynamicQuery, Evaluate, JevConfig};
use serde_json::Value;

use crate::answer::{DecisionAnswer, DecisionOutcome};
use crate::decision::{Decision, DecisionApiType, MAX_QUESTIONS, MAX_STATE_BYTES};
use crate::error::{DecisionError, Result};

/// The credential sent to servers that ignore authentication (Ollama's local
/// System One endpoint does). The request shape always carries an
/// `Authorization` header, and rig refuses an empty credential, so an
/// unauthenticated connection still needs a placeholder.
pub const PLACEHOLDER_TOKEN: &str = "systemone";

/// A decision connection: one System One endpoint, its credential, and the
/// model to evaluate with.
#[derive(Debug, Clone)]
pub struct DecisionClient {
    /// The configured connection id, used to label errors. The wire is shared
    /// by every backend, so the id is the actionable half of a failure.
    label: String,
    api_type: DecisionApiType,
    model: String,
    endpoint: String,
    token: String,
    /// Whether the token is a configured credential rather than the
    /// placeholder. An authentication failure on a placeholder gets a hint
    /// about the missing key.
    authenticated: bool,
}

impl DecisionClient {
    /// Build a client for `endpoint`, which is either a host root (the System
    /// One path is appended) or a complete URL used as written.
    pub fn build(
        api_type: DecisionApiType,
        label: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
        api_key: Option<&str>,
    ) -> Result<Self> {
        let label = label.into();
        let model = model.into();
        let endpoint = endpoint.into();
        let fail = |message: &str| DecisionError::Provider {
            provider: label.clone(),
            message: message.to_string(),
        };
        if model.trim().is_empty() {
            return Err(fail("no model is selected for this decision provider"));
        }
        // Checked before normalization, which would turn a blank URL into a
        // bare protocol path.
        if endpoint.trim().is_empty() {
            return Err(fail("no endpoint is configured for this decision provider"));
        }
        let endpoint = system_one_endpoint(&endpoint);
        let api_key = api_key.map(str::trim).filter(|key| !key.is_empty());
        Ok(Self {
            label,
            api_type,
            model,
            endpoint,
            token: api_key.unwrap_or(PLACEHOLDER_TOKEN).to_string(),
            authenticated: api_key.is_some(),
        })
    }

    /// The connection id this client reports errors under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The model this client evaluates with.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Evaluate one state against one or more decisions in a single request.
    /// Every decision answers the same state independently.
    pub async fn evaluate(&self, state: &Value, decisions: &[Decision]) -> Result<DecisionOutcome> {
        if decisions.is_empty() {
            return Err(self.fail("at least one decision is required"));
        }
        if decisions.len() > MAX_QUESTIONS {
            return Err(self.fail(format!(
                "at most {MAX_QUESTIONS} decisions fit one request, found {}",
                decisions.len()
            )));
        }
        let mut definitions = BTreeMap::new();
        for decision in decisions {
            decision.validate()?;
            if definitions
                .insert(decision.name.clone(), decision.question())
                .is_some()
            {
                return Err(decision.invalid("duplicate decision name in one request"));
            }
        }
        let size = serde_json::to_vec(state)
            .map_err(|error| self.fail(format!("state is not serializable: {error}")))?
            .len();
        if size > MAX_STATE_BYTES {
            return Err(DecisionError::StateTooLarge {
                size,
                limit: MAX_STATE_BYTES,
            });
        }
        let query = DynamicQuery::new(definitions).map_err(|error| self.fail(error.to_string()))?;
        let result = JevConfig::new(self.token.clone())
            .model(self.model.clone())
            .with_endpoint(self.endpoint.clone())
            .client()
            .evaluation()
            .evaluate(state, query)
            .await
            .map_err(|error| self.provider_error(&error))?;
        let mut answers = BTreeMap::new();
        for (name, answer) in result.answers {
            answers.insert(name.clone(), DecisionAnswer::from_wire(&name, answer)?);
        }
        Ok(DecisionOutcome {
            answers,
            model: result.model,
            usage: result.usage,
            request_id: result.provider_request_id,
        })
    }

    /// Evaluate one decision and return its answer.
    pub async fn decide(&self, state: &Value, decision: &Decision) -> Result<DecisionAnswer> {
        let outcome = self.evaluate(state, std::slice::from_ref(decision)).await?;
        outcome
            .answers
            .into_values()
            .next()
            .ok_or_else(|| self.fail(format!("no answer for decision `{}`", decision.name)))
    }

    fn fail(&self, message: impl Into<String>) -> DecisionError {
        DecisionError::Provider {
            provider: self.label.clone(),
            message: message.into(),
        }
    }

    /// Map a wire failure onto an actionable message. `rig-typesafeai` names
    /// itself `typesafeai` in its descriptor, which would mislabel an Ollama or
    /// Cloudflare failure, so the configured connection id leads instead.
    fn provider_error(&self, error: &ProviderError) -> DecisionError {
        let mut message = format!(
            "{} at `{}` (model `{}`, {})",
            error,
            self.endpoint,
            self.model,
            self.api_type.as_str()
        );
        if let Some(body) = error.provider_response_body() {
            let body = body.trim();
            if !body.is_empty() {
                message.push_str(": ");
                message.push_str(&truncate(body, 512));
            }
        }
        if is_authentication_failure(error) && !self.authenticated {
            message.push_str(" — no api-key is configured for this decision provider");
        }
        self.fail(message)
    }
}

/// Whether a reply rejected the credential. `rig-typesafeai`'s shared HTTP
/// driver surfaces a rejected credential as a preserved provider response
/// rather than `ProviderError::InvalidAuthentication`, so the status is read
/// directly and the variant is accepted too, whichever way the failure arrives.
fn is_authentication_failure(error: &ProviderError) -> bool {
    if matches!(error, ProviderError::InvalidAuthentication(_)) {
        return true;
    }
    error.provider_response_status().is_some_and(|status| {
        status == http::StatusCode::UNAUTHORIZED || status == http::StatusCode::FORBIDDEN
    })
}

/// The System One endpoint for a configured URL: a host root gains the
/// protocol path, while a complete URL — TypeSafe's `/v1/systemone`, or a path
/// that already names an endpoint such as Cloudflare's Workers AI route — is
/// used as written.
fn system_one_endpoint(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let authority_end = match url.find("://") {
        Some(index) => index + "://".len(),
        None => 0,
    };
    let has_path = url
        .get(authority_end..)
        .is_some_and(|rest| rest.contains('/'));
    if has_path {
        url.to_string()
    } else {
        format!("{url}/v1/systemone")
    }
}

/// Truncate on a character boundary, marking the cut so a shortened provider
/// body is never mistaken for the whole reply.
fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text.get(..end).unwrap_or_default())
}

#[cfg(test)]
mod tests;
