use std::collections::HashMap;

use tokio::sync::mpsc::Sender;

use shuvarie_db::Store;
use shuvarie_llm::ProviderClient;

use crate::event::Event;
use shuvarie_config::Config;
use shuvarie_config::Connections;

const MAX_TEXT_CHARS: usize = 8000;
const BATCH_SIZE: u64 = 64;

#[derive(Clone)]
pub struct EmbeddingSetup {
    pub client: ProviderClient,
    pub model: String,
    pub dims: usize,
}

pub fn setup(
    config: &Config,
    connections: &Connections,
    clients: &mut HashMap<String, ProviderClient>,
    event_tx: &Sender<Event>,
) -> Option<EmbeddingSetup> {
    if config.embedding.disabled {
        return None;
    }
    let provider_name = config
        .embedding
        .provider
        .clone()
        .or_else(|| connections.active.as_ref().map(|a| a.provider.clone()))?;
    let pc = connections.providers.get(&provider_name)?;
    let kind = crate::catalog::provider_type(&pc.kind);
    let client = match clients.get(&provider_name) {
        Some(c) => c.clone(),
        None => {
            let base_url = crate::catalog::base_url_for(&pc.kind, pc.base_url.as_deref());
            let on_device_code =
                crate::core_task::device_code_handler(kind, pc.name.clone(), event_tx);
            let c = ProviderClient::build_with_device_code(
                kind,
                pc.api_key.as_deref(),
                base_url.as_deref(),
                on_device_code,
            )
            .ok()?;
            clients.insert(provider_name.clone(), c.clone());
            c
        }
    };
    if !client.supports_embeddings() {
        return None;
    }
    let model = config
        .embedding
        .model
        .clone()
        .unwrap_or_else(|| default_model(kind));
    let dims = config
        .embedding
        .dimensions
        .map(|d| d as usize)
        .unwrap_or_else(|| default_dims(kind));
    Some(EmbeddingSetup {
        client,
        model,
        dims,
    })
}

pub fn default_model(kind: selune::ProviderType) -> String {
    match kind {
        selune::ProviderType::Ollama => "nomic-embed-text".to_string(),
        selune::ProviderType::Google => "gemini-embedding-001".to_string(),
        selune::ProviderType::Cohere => "embed-english-v4.0".to_string(),
        selune::ProviderType::Mistral => "mistral-embed".to_string(),
        selune::ProviderType::Voyageai => "voyage-3.5".to_string(),
        selune::ProviderType::Llamafile => "LLaMA_CPP".to_string(),
        _ => "text-embedding-3-small".to_string(),
    }
}

pub fn default_dims(kind: selune::ProviderType) -> usize {
    match kind {
        selune::ProviderType::Ollama => 768,
        selune::ProviderType::Cohere => 1536,
        selune::ProviderType::Mistral => 1024,
        selune::ProviderType::Voyageai => 1024,
        _ => 1536,
    }
}

pub async fn index_message(
    store: &mut Store,
    setup: &EmbeddingSetup,
    message_id: u64,
    session_id: uuid::Uuid,
    seq: u64,
    content: &str,
) -> Result<(), String> {
    if content.chars().count() > MAX_TEXT_CHARS {
        return Ok(());
    }
    let texts = vec![content.to_string()];
    let mut vecs = setup
        .client
        .embed(&setup.model, setup.dims, &texts)
        .await
        .map_err(|e| e.to_string())?;
    let Some(vec) = vecs.pop() else {
        return Ok(());
    };
    store
        .upsert_embedding(message_id, session_id, seq, content, f32_blob(&vec))
        .await
        .map_err(|e| e.to_string())
}

pub async fn backfill(store: &mut Store, setup: &EmbeddingSetup) {
    loop {
        let Ok(batch) = store.messages_missing_embeddings(BATCH_SIZE).await else {
            return;
        };
        if batch.is_empty() {
            return;
        }
        let mut texts: Vec<String> = Vec::with_capacity(batch.len());
        for m in &batch {
            if m.content.chars().count() <= MAX_TEXT_CHARS {
                texts.push(m.content.clone());
            }
        }
        let Ok(vecs) = setup.client.embed(&setup.model, setup.dims, &texts).await else {
            return;
        };
        let mut idx = 0;
        for m in &batch {
            if m.content.chars().count() > MAX_TEXT_CHARS {
                continue;
            }
            let Some(vec) = vecs.get(idx) else {
                continue;
            };
            idx += 1;
            let _ = store
                .upsert_embedding(m.id, m.session_id, m.seq, &m.content, f32_blob(vec))
                .await;
        }
    }
}

fn f32_blob(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn rrf_merge(
    fts: Vec<shuvarie_db::SearchHit>,
    semantic: Vec<shuvarie_db::SearchHit>,
    k: u64,
    limit: usize,
) -> Vec<shuvarie_db::SearchHit> {
    use std::collections::HashMap;
    let mut scores: HashMap<u64, (f64, shuvarie_db::SearchHit)> = HashMap::new();
    for (rank, hit) in fts.into_iter().enumerate() {
        let entry = scores
            .entry(hit.message_id)
            .or_insert_with(|| (0.0, hit.clone()));
        entry.0 += 1.0 / (k as f64 + rank as f64 + 1.0);
    }
    for (rank, hit) in semantic.into_iter().enumerate() {
        let entry = scores
            .entry(hit.message_id)
            .or_insert_with(|| (0.0, hit.clone()));
        entry.0 += 1.0 / (k as f64 + rank as f64 + 1.0);
    }
    let mut out: Vec<(f64, shuvarie_db::SearchHit)> = scores.into_values().collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter().take(limit).map(|(_, h)| h).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_merges_and_boosts_common_hits() {
        let hit = |id: u64| shuvarie_db::SearchHit {
            message_id: id,
            session_id: uuid::Uuid::from_u128(1),
            seq: 0,
            role: shuvarie_db::MsgRole::User,
            content: "x".into(),
            session_title: "t".into(),
            score: 0.0,
            source: shuvarie_db::SearchSource::Fts,
        };
        let fts = vec![hit(1), hit(2)];
        let semantic = vec![hit(2), hit(3)];
        let merged = rrf_merge(fts, semantic, 60, 10);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].message_id, 2, "present in both ranks first");
    }

    #[test]
    fn rrf_limit_applies() {
        let hit = |id: u64| shuvarie_db::SearchHit {
            message_id: id,
            session_id: uuid::Uuid::from_u128(1),
            seq: 0,
            role: shuvarie_db::MsgRole::User,
            content: "x".into(),
            session_title: "t".into(),
            score: 0.0,
            source: shuvarie_db::SearchSource::Fts,
        };
        let fts = vec![hit(1), hit(2), hit(3)];
        let merged = rrf_merge(fts, vec![], 60, 2);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn defaults_per_provider() {
        use selune::ProviderType::*;
        assert_eq!(default_model(Ollama), "nomic-embed-text");
        assert_eq!(default_model(OpenaiCompat), "text-embedding-3-small");
        assert_eq!(default_dims(Ollama), 768);
    }
}
