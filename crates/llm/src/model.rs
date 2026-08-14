use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
}

impl ModelInfo {
    pub fn from_rig(m: &rig::model::Model) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            context_length: m.context_length.map(u64::from),
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_ref().unwrap_or(&self.id)
    }
}
