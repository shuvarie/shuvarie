use serde::{Deserialize, Serialize};

/// A lightweight, portable description of an available model, produced by
/// [`crate::provider::ProviderClient::list_models`]. Rig types stay out of
/// `shuvarie-core`; this is the boundary type the TUI renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
}

impl ModelInfo {
    pub fn display_name(&self) -> &str {
        self.name.as_ref().unwrap_or(&self.id)
    }
}

pub fn model_info_from_rig(m: &rig::model::Model) -> ModelInfo {
    ModelInfo {
        id: m.id.clone(),
        name: m.name.clone(),
        context_length: m.context_length.map(u64::from),
    }
}
