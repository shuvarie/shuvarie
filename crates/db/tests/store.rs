use shuvarie_db::{Store, StoredSession};
use shuvarie_llm::{Role, TokenUsage};

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
