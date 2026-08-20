use shuvarie_catalog::ModelInfo;

pub fn model_info_from_rig(m: &rig::model::Model) -> ModelInfo {
    ModelInfo {
        id: m.id.clone(),
        name: m.name.clone(),
        context_length: m.context_length.map(u64::from),
    }
}
