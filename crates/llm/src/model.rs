pub use rig::model::Model;

pub fn display_name(m: &Model) -> &str {
    m.name.as_deref().unwrap_or(&m.id)
}
