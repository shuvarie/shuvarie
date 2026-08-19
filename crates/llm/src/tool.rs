use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;
}
