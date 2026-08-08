use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: Option<String>,
}

impl ModelInfo {
    pub fn from_rig(m: &rig::model::Model) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_ref().unwrap_or(&self.id)
    }
}
