use std::sync::Arc;

/// Device authorization details surfaced to a provider callback while an
/// OAuth-backed client (ChatGPT, Copilot) waits for the user to authorize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCodePrompt {
    /// URL where the user authorizes the device.
    pub verification_uri: String,
    /// Short code the user enters at the verification URL.
    pub user_code: String,
}

/// Callback invoked when an OAuth device flow needs user action: visit
/// `verification_uri` and enter `user_code`, then the provider picks the
/// session up automatically while it polls.
pub type DeviceCodeHandler = Arc<dyn Fn(DeviceCodePrompt) + Send + Sync>;
