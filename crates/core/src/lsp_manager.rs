use std::sync::Arc;

use tokio::sync::Mutex;

pub type SharedManager = Arc<Mutex<shuvarie_lsp::LspManager>>;
