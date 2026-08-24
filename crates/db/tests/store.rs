use shuvarie_catalog::TokenUsage;
use shuvarie_db::{Store, StoredSession};
use shuvarie_llm::Role;

#[tokio::test]
async fn create_and_list_sessions_most_recent_first() {
    let mut store = Store::open_in_memory().await.unwrap();
    let s1 = store.create_session("first", None, None).await.unwrap();
    let s2 = store
        .create_session("second", Some("ollama"), Some("model-x"))
        .await
        .unwrap();

    let list = store.list_sessions().await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, s2, "most recently updated first");
    assert_eq!(list[1].id, s1);
    assert_eq!(list[0].title, "second");
    assert_eq!(list[0].message_count, 0);
}

#[tokio::test]
async fn append_and_load_messages_in_order() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None).await.unwrap();

    store.append_message(id, Role::User, "hello").await.unwrap();
    store
        .append_assistant_message(
            id,
            "hi there",
            "",
            false,
            TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                cached_input_tokens: 4,
                reasoning_tokens: 5,
            },
            0.0012,
        )
        .await
        .unwrap();
    store.append_message(id, Role::User, "again").await.unwrap();

    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages.len(), 3);
    assert_eq!(loaded.messages[0].content, "hello");
    assert_eq!(loaded.messages[0].seq, 0);
    assert_eq!(loaded.messages[1].content, "hi there");
    assert_eq!(loaded.messages[1].seq, 1);
    assert_eq!(loaded.messages[1].input_tokens, 10);
    assert_eq!(loaded.messages[1].output_tokens, 20);
    assert_eq!(loaded.messages[1].total_tokens, 30);
    assert_eq!(loaded.messages[1].cached_input_tokens, 4);
    assert_eq!(loaded.messages[1].reasoning_tokens, 5);
    assert_eq!(loaded.messages[1].cost, 0.0012);
    assert_eq!(loaded.messages[2].content, "again");
    assert_eq!(loaded.messages[2].seq, 2);
}

#[tokio::test]
async fn role_round_trip() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("roles", None, None).await.unwrap();
    store.append_message(id, Role::System, "sys").await.unwrap();
    store.append_message(id, Role::User, "usr").await.unwrap();
    store
        .append_message(id, Role::Assistant, "ast")
        .await
        .unwrap();

    let loaded = store.load_session(id).await.unwrap();
    let roles: Vec<Role> = loaded
        .messages
        .iter()
        .map(|m| m.role.clone().into())
        .collect();
    assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant]);
}

#[tokio::test]
async fn most_recent_session_is_last_updated() {
    let mut store = Store::open_in_memory().await.unwrap();
    assert!(store.most_recent_session().await.unwrap().is_none());

    let old = store.create_session("old", None, None).await.unwrap();
    let new = store.create_session("new", None, None).await.unwrap();
    store
        .append_message(old, Role::User, "touching old")
        .await
        .unwrap();

    let recent = store.most_recent_session().await.unwrap().unwrap();
    assert_eq!(recent.id, old, "old was updated last");
    assert_eq!(recent.title, "old");

    store
        .append_message(new, Role::User, "touching new")
        .await
        .unwrap();
    let recent = store.most_recent_session().await.unwrap().unwrap();
    assert_eq!(recent.id, new);
}

#[tokio::test]
async fn delete_session_removes_messages() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("gone", None, None).await.unwrap();
    store.append_message(id, Role::User, "x").await.unwrap();

    store.delete_session(id).await.unwrap();
    assert!(store.load_session(id).await.is_err());
    assert!(store.list_sessions().await.unwrap().is_empty());
}

#[tokio::test]
async fn load_missing_session_errors() {
    let mut store = Store::open_in_memory().await.unwrap();
    let err = store.load_session(999).await.unwrap_err();
    assert!(matches!(err, shuvarie_db::DbError::NotFound { id: 999 }));
}

#[tokio::test]
async fn reopen_applies_migrations_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");

    let id = {
        let mut store = Store::open(&path).await.unwrap();
        let id = store
            .create_session("persisted", Some("ollama"), Some("m1"))
            .await
            .unwrap();
        store.append_message(id, Role::User, "hello").await.unwrap();
        id
    };

    let mut store = Store::open(&path).await.unwrap();
    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(loaded.title, "persisted");
    assert_eq!(loaded.provider.as_deref(), Some("ollama"));
    assert_eq!(loaded.model.as_deref(), Some("m1"));
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.messages[0].content, "hello");
}

#[tokio::test]
async fn search_finds_messages_across_sessions_ranked() {
    let mut store = Store::open_in_memory().await.unwrap();
    let s1 = store.create_session("rust", None, None).await.unwrap();
    let s2 = store.create_session("sql", None, None).await.unwrap();

    store
        .append_message(s1, Role::User, "how does tokio spawn tasks?")
        .await
        .unwrap();
    store
        .append_assistant_message(
            s1,
            "tokio::spawn runs a task on the runtime",
            "",
            false,
            TokenUsage::default(),
            0.0,
        )
        .await
        .unwrap();
    store
        .append_message(s2, Role::User, "how does sqlite index rows?")
        .await
        .unwrap();

    let hits = store.search_messages("tokio", 10).await.unwrap();
    assert_eq!(hits.len(), 2, "both tokio messages match");
    assert!(hits.iter().all(|h| h.session_id == s1));
    assert!(hits[0].score >= hits[1].score, "ranked by relevance");

    let hits = store.search_messages("sqlite", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, s2);
    assert_eq!(hits[0].session_title, "sql");
    assert_eq!(hits[0].content, "how does sqlite index rows?");
    assert_eq!(hits[0].seq, 0);
    assert!(matches!(hits[0].role, shuvarie_db::MsgRole::User));
}

#[tokio::test]
async fn search_is_case_insensitive_and_empty_query_no_match() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None).await.unwrap();
    store
        .append_message(id, Role::User, "Rust borrow checker")
        .await
        .unwrap();

    let hits = store.search_messages("borrow", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    let hits = store.search_messages("RUST", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    let hits = store.search_messages("garbage", 10).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn search_respects_limit_and_delete() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None).await.unwrap();
    for i in 0..5 {
        store
            .append_message(id, Role::User, &format!("request number {i}"))
            .await
            .unwrap();
    }

    let hits = store.search_messages("request", 3).await.unwrap();
    assert_eq!(hits.len(), 3);

    store.delete_session(id).await.unwrap();
    let hits = store.search_messages("request", 10).await.unwrap();
    assert!(hits.is_empty(), "deleted sessions leave no hits");
}

fn vec_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[tokio::test]
async fn upsert_embedding_roundtrip_and_upsert() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None).await.unwrap();
    store
        .append_message(id, Role::User, "how do I parse json?")
        .await
        .unwrap();
    let msg = store.load_session(id).await.unwrap();
    let mid = msg.messages[0].id;

    store
        .upsert_embedding(mid, id, 0, "how do I parse json?", vec_bytes(&[1.0, 2.0]))
        .await
        .unwrap();
    store
        .upsert_embedding(mid, id, 0, "how do I parse json?", vec_bytes(&[3.0, 4.0]))
        .await
        .unwrap();

    let missing = store.messages_missing_embeddings(10).await.unwrap();
    assert!(missing.is_empty(), "upsert should replace, not duplicate");
}

#[tokio::test]
async fn missing_embeddings_lists_only_unembedded() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None).await.unwrap();
    store.append_message(id, Role::User, "a").await.unwrap();
    store.append_message(id, Role::User, "b").await.unwrap();
    let loaded = store.load_session(id).await.unwrap();

    store
        .upsert_embedding(loaded.messages[0].id, id, 0, "a", vec_bytes(&[0.5]))
        .await
        .unwrap();

    let missing = store.messages_missing_embeddings(10).await.unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].content, "b");
    assert_eq!(missing[0].seq, 1);

    let empty = store.messages_missing_embeddings(0).await.unwrap();
    assert!(empty.is_empty(), "limit 0 yields nothing");
}

#[tokio::test]
async fn semantic_search_ranks_by_cosine() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("rust", None, None).await.unwrap();
    store
        .append_message(id, Role::User, "tokio spawn")
        .await
        .unwrap();
    store
        .append_message(id, Role::User, "sqlite index")
        .await
        .unwrap();
    let loaded = store.load_session(id).await.unwrap();

    store
        .upsert_embedding(
            loaded.messages[0].id,
            id,
            0,
            "tokio spawn",
            vec_bytes(&[1.0, 0.0]),
        )
        .await
        .unwrap();
    store
        .upsert_embedding(
            loaded.messages[1].id,
            id,
            1,
            "sqlite index",
            vec_bytes(&[0.0, 1.0]),
        )
        .await
        .unwrap();

    let hits = store.semantic_search(vec![0.9, 0.1], 10).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].content, "tokio spawn", "closest vector first");
    assert_eq!(hits[0].session_title, "rust");
    assert_eq!(hits[0].seq, 0);
    assert!(hits[0].score <= hits[1].score, "distance ascending");
}

#[tokio::test]
async fn semantic_search_respects_limit_and_delete() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None).await.unwrap();
    store.append_message(id, Role::User, "alpha").await.unwrap();
    store.append_message(id, Role::User, "beta").await.unwrap();
    let loaded = store.load_session(id).await.unwrap();
    for (i, m) in loaded.messages.iter().enumerate() {
        store
            .upsert_embedding(m.id, id, i as u64, &m.content, vec_bytes(&[i as f32]))
            .await
            .unwrap();
    }

    assert_eq!(store.semantic_search(vec![0.0], 1).await.unwrap().len(), 1);

    store.delete_session(id).await.unwrap();
    assert!(
        store
            .semantic_search(vec![0.0], 10)
            .await
            .unwrap()
            .is_empty(),
        "deleted session leaves no embeddings"
    );
}
