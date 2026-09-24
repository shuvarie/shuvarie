use shuvarie_db::{
    LockAcquire, ReasoningSegment, SESSION_LOCK_TTL_MS, Store, StoredScroll, StoredSession,
    WORKSPACE_DIR_NAME,
};
use shuvarie_llm::Role;
use shuvarie_llm::TokenUsage;

#[tokio::test]
async fn open_seeds_gitignore_when_creating_workspace_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(WORKSPACE_DIR_NAME).join("data.db");

    Store::open(&path).await.unwrap();
    let gitignore = dir.path().join(WORKSPACE_DIR_NAME).join(".gitignore");
    assert!(gitignore.exists());
    assert_eq!(std::fs::read_to_string(&gitignore).unwrap(), "*\n");

    std::fs::write(&gitignore, "!data.db\n").unwrap();
    Store::open(&path).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&gitignore).unwrap(),
        "!data.db\n",
        "reopen must not overwrite an existing .gitignore"
    );

    let plain = dir.path().join("other");
    std::fs::create_dir_all(&plain).unwrap();
    Store::open(&plain.join("data.db")).await.unwrap();
    assert!(!plain.join(".gitignore").exists());
}

#[tokio::test]
async fn create_and_list_sessions_most_recent_first() {
    let mut store = Store::open_in_memory().await.unwrap();
    let s1 = store
        .create_session("first", None, None, None)
        .await
        .unwrap();
    let s2 = store
        .create_session("second", Some("ollama"), Some("model-x"), None)
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
async fn worker_sessions_are_hidden_from_list_and_most_recent() {
    let mut store = Store::open_in_memory().await.unwrap();
    let main = store
        .create_session("main", None, None, None)
        .await
        .unwrap();
    store
        .create_worker_session("explore_workspace", Some("ollama"), Some("model-x"), main)
        .await
        .unwrap();

    let list = store.list_sessions().await.unwrap();
    assert_eq!(list.len(), 1, "worker sessions must not appear in the list");
    assert_eq!(list[0].id, main);

    let recent = store.most_recent_session().await.unwrap().unwrap();
    assert_eq!(recent.id, main, "worker sessions must not be auto-resumed");
}

#[tokio::test]
async fn append_and_load_messages_in_order() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None, None).await.unwrap();

    store
        .append_message(id, None, Role::User, "hello")
        .await
        .unwrap();
    store
        .append_assistant_message(
            id,
            None,
            "hi there",
            &[],
            &[],
            false,
            TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                cached_input_tokens: 4,
                reasoning_tokens: 5,
                ..Default::default()
            },
            0.0012,
            &TokenUsage {
                input_tokens: 12_000,
                output_tokens: 20,
                total_tokens: 12_020,
                cached_input_tokens: 11_500,
                ..Default::default()
            },
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "again")
        .await
        .unwrap();

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
    assert_eq!(loaded.messages[1].request.input_tokens, 12_000);
    assert_eq!(loaded.messages[1].request.total_tokens, 12_020);
    assert_eq!(loaded.messages[1].request.cached_input_tokens, 11_500);
    assert_eq!(loaded.messages[2].content, "again");
    assert_eq!(loaded.messages[2].seq, 2);
}

#[tokio::test]
async fn role_round_trip() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("roles", None, None, None)
        .await
        .unwrap();
    store
        .append_message(id, None, Role::System, "sys")
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "usr")
        .await
        .unwrap();
    store
        .append_message(id, None, Role::Assistant, "ast")
        .await
        .unwrap();

    let loaded = store.load_session(id).await.unwrap();
    let roles: Vec<Role> = loaded.messages.iter().map(|m| m.role.into()).collect();
    assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant]);
}

#[tokio::test]
async fn most_recent_session_is_last_updated() {
    let mut store = Store::open_in_memory().await.unwrap();
    assert!(store.most_recent_session().await.unwrap().is_none());

    let old = store.create_session("old", None, None, None).await.unwrap();
    let new = store.create_session("new", None, None, None).await.unwrap();
    store
        .append_message(old, None, Role::User, "touching old")
        .await
        .unwrap();

    let recent = store.most_recent_session().await.unwrap().unwrap();
    assert_eq!(recent.id, old, "old was updated last");
    assert_eq!(recent.title, "old");

    store
        .append_message(new, None, Role::User, "touching new")
        .await
        .unwrap();
    let recent = store.most_recent_session().await.unwrap().unwrap();
    assert_eq!(recent.id, new);
}

#[tokio::test]
async fn reasoning_segments_round_trip_with_positions() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("think", None, None, None)
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "do it")
        .await
        .unwrap();

    let segments = vec![
        ReasoningSegment {
            after_tool: 0,
            text: "first thoughts".to_string(),
            duration_ms: 0,
        },
        ReasoningSegment {
            after_tool: 2,
            text: "thoughts after two tools".to_string(),
            duration_ms: 0,
        },
    ];
    let msg = store
        .append_assistant_message(
            id,
            None,
            "done",
            &segments,
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    assert_eq!(msg.reasoning, segments);

    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages[1].reasoning, segments);

    store
        .update_message(
            msg.id,
            "done edited",
            &segments,
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages[1].reasoning, segments);
}

#[tokio::test]
async fn text_segments_round_trip_with_positions() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("runs", None, None, None)
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "do it")
        .await
        .unwrap();

    let segments = vec![
        shuvarie_db::TextSegment {
            after_tool: 0,
            text: "first run".to_string(),
        },
        shuvarie_db::TextSegment {
            after_tool: 2,
            text: "run after two tools".to_string(),
        },
    ];
    let msg = store
        .append_assistant_message(
            id,
            None,
            "first run\n\nrun after two tools",
            &[],
            &segments,
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    assert_eq!(msg.text_segments, segments);

    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages[1].text_segments, segments);

    store
        .update_message(
            msg.id,
            "edited",
            &[],
            &segments,
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages[1].text_segments, segments);
}

#[tokio::test]
async fn delete_session_removes_messages() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("gone", None, None, None)
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "x")
        .await
        .unwrap();

    store.delete_session(id).await.unwrap();
    assert!(store.load_session(id).await.is_err());
    assert!(store.list_sessions().await.unwrap().is_empty());
}

#[tokio::test]
async fn load_missing_session_errors() {
    let mut store = Store::open_in_memory().await.unwrap();
    let missing = uuid::Uuid::nil();
    let err = store.load_session(missing).await.unwrap_err();
    assert!(matches!(err, shuvarie_db::DbError::NotFound { id } if id == missing));
}

#[tokio::test]
async fn reopen_applies_migrations_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");

    let id = {
        let mut store = Store::open(&path).await.unwrap();
        let id = store
            .create_session("persisted", Some("ollama"), Some("m1"), None)
            .await
            .unwrap();
        store
            .append_message(id, None, Role::User, "hello")
            .await
            .unwrap();
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
async fn set_scroll_persists_and_loads_without_touching_updated_at() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("t", None, None, None).await.unwrap();
    store
        .append_message(id, None, Role::User, "hello")
        .await
        .unwrap();
    let before = store.list_sessions().await.unwrap()[0].updated_at_epoch_ms;

    store
        .set_scroll(
            id,
            StoredScroll {
                sticky: false,
                anchor: Some((3, 41)),
            },
        )
        .await
        .unwrap();
    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(
        loaded.scroll,
        StoredScroll {
            sticky: false,
            anchor: Some((3, 41)),
        }
    );
    assert_eq!(
        store.list_sessions().await.unwrap()[0].updated_at_epoch_ms,
        before,
        "a scroll write must not reorder the session list"
    );

    store.set_scroll(id, StoredScroll::default()).await.unwrap();
    let loaded: StoredSession = store.load_session(id).await.unwrap();
    assert_eq!(loaded.scroll, StoredScroll::default());
}

#[tokio::test]
async fn set_title_renames_without_touching_updated_at() {
    let mut store = Store::open_in_memory().await.unwrap();
    let first = store
        .create_session("first", None, None, None)
        .await
        .unwrap();
    let second = store
        .create_session("second", None, None, None)
        .await
        .unwrap();
    store
        .append_message(first, None, Role::User, "bump first")
        .await
        .unwrap();
    let second_before = store
        .list_sessions()
        .await
        .unwrap()
        .iter()
        .find(|s| s.id == second)
        .unwrap()
        .updated_at_epoch_ms;

    store.set_title(second, "renamed").await.unwrap();
    let loaded = store.load_session(second).await.unwrap();
    assert_eq!(loaded.title, "renamed");

    let list = store.list_sessions().await.unwrap();
    let entry = list.iter().find(|s| s.id == second).unwrap();
    assert_eq!(entry.title, "renamed");
    assert_eq!(
        entry.updated_at_epoch_ms, second_before,
        "a rename must not reorder the session list"
    );
    assert_eq!(list[0].id, first, "order unchanged by the rename");
}

#[tokio::test]
async fn set_title_if_swaps_only_when_title_is_unchanged() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("provisional", None, None, None)
        .await
        .unwrap();
    let other = store
        .create_session("other", None, None, None)
        .await
        .unwrap();

    // A mismatched expectation (including the wrong session's title) is a
    // no-op that reports `false`.
    assert!(
        !store
            .set_title_if(id, "not the title", "generated")
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_title_if(other, "provisional", "generated")
            .await
            .unwrap()
    );
    assert_eq!(store.load_session(id).await.unwrap().title, "provisional");

    // The matching expectation swaps the title.
    assert!(
        store
            .set_title_if(id, "provisional", "generated")
            .await
            .unwrap()
    );
    assert_eq!(store.load_session(id).await.unwrap().title, "generated");

    // After the swap the old expectation no longer matches, so a repeated
    // (racing) write cannot clobber a manual rename in between.
    store.set_title(id, "manual rename").await.unwrap();
    assert!(
        !store
            .set_title_if(id, "provisional", "generated")
            .await
            .unwrap()
    );
    assert_eq!(store.load_session(id).await.unwrap().title, "manual rename");
}

#[tokio::test]
async fn search_finds_messages_across_sessions_ranked() {
    let mut store = Store::open_in_memory().await.unwrap();
    let s1 = store
        .create_session("rust", None, None, None)
        .await
        .unwrap();
    let s2 = store.create_session("sql", None, None, None).await.unwrap();

    store
        .append_message(s1, None, Role::User, "how does tokio spawn tasks?")
        .await
        .unwrap();
    store
        .append_assistant_message(
            s1,
            None,
            "tokio::spawn runs a task on the runtime",
            &[],
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    store
        .append_message(s2, None, Role::User, "how does sqlite index rows?")
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
    let id = store.create_session("t", None, None, None).await.unwrap();
    store
        .append_message(id, None, Role::User, "Rust borrow checker")
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
    let id = store.create_session("t", None, None, None).await.unwrap();
    for i in 0..5 {
        store
            .append_message(id, None, Role::User, &format!("request number {i}"))
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
    let id = store.create_session("t", None, None, None).await.unwrap();
    store
        .append_message(id, None, Role::User, "how do I parse json?")
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
    let id = store.create_session("t", None, None, None).await.unwrap();
    store
        .append_message(id, None, Role::User, "a")
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "b")
        .await
        .unwrap();
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
    let id = store
        .create_session("rust", None, None, None)
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "tokio spawn")
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "sqlite index")
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
    let id = store.create_session("t", None, None, None).await.unwrap();
    store
        .append_message(id, None, Role::User, "alpha")
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "beta")
        .await
        .unwrap();
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[tokio::test]
async fn session_lock_acquire_refresh_and_release() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("data.db"))
        .await
        .unwrap()
        .with_client_id("a");
    let id = store
        .create_session("locked", None, None, None)
        .await
        .unwrap();
    let base = now_ms();

    assert_eq!(
        store.acquire_session_lock(id, base).await.unwrap(),
        LockAcquire::Acquired
    );
    assert_eq!(
        store.acquire_session_lock(id, base + 5_000).await.unwrap(),
        LockAcquire::Ours
    );

    store.release_session_lock(id).await.unwrap();
    assert_eq!(
        store.acquire_session_lock(id, base + 6_000).await.unwrap(),
        LockAcquire::Acquired
    );
}

#[tokio::test]
async fn session_lock_held_until_ttl_then_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let id = {
        let mut a = Store::open(&path).await.unwrap().with_client_id("a");
        let id = a.create_session("held", None, None, None).await.unwrap();
        a.acquire_session_lock(id, now_ms()).await.unwrap();
        id
    };

    let mut b = Store::open(&path).await.unwrap().with_client_id("b");
    assert_eq!(
        b.acquire_session_lock(id, now_ms()).await.unwrap(),
        LockAcquire::Held
    );
    assert_eq!(
        b.acquire_session_lock(id, now_ms() + SESSION_LOCK_TTL_MS + 1)
            .await
            .unwrap(),
        LockAcquire::Acquired,
        "stale lock is taken over"
    );
}

#[tokio::test]
async fn touch_session_lock_reports_lost_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let id = {
        let mut a = Store::open(&path).await.unwrap().with_client_id("a");
        let id = a.create_session("stale", None, None, None).await.unwrap();
        a.acquire_session_lock(id, now_ms() - SESSION_LOCK_TTL_MS - 1)
            .await
            .unwrap();
        id
    };

    let mut b = Store::open(&path).await.unwrap().with_client_id("b");
    b.acquire_session_lock(id, now_ms()).await.unwrap();

    let mut a = Store::open(&path).await.unwrap().with_client_id("a");
    assert!(!a.touch_session_lock(id, now_ms()).await.unwrap());
}

#[tokio::test]
async fn locked_by_other_reflects_holder_and_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let base = now_ms();
    let id = {
        let mut a = Store::open(&path).await.unwrap().with_client_id("a");
        let id = a.create_session("held", None, None, None).await.unwrap();
        a.acquire_session_lock(id, base).await.unwrap();
        id
    };

    let mut b = Store::open(&path).await.unwrap().with_client_id("b");
    assert!(b.locked_by_other(id, base + 1_000).await.unwrap());
    assert!(
        !b.locked_by_other(id, base + SESSION_LOCK_TTL_MS + 1)
            .await
            .unwrap()
    );

    b.acquire_session_lock(id, base + SESSION_LOCK_TTL_MS + 2)
        .await
        .unwrap();
    assert!(
        !b.locked_by_other(id, base + SESSION_LOCK_TTL_MS + 2)
            .await
            .unwrap(),
        "own lock"
    );

    let mut anonymous = Store::open(&path).await.unwrap();
    assert!(
        anonymous
            .locked_by_other(id, base + SESSION_LOCK_TTL_MS + 3)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn list_sessions_reports_in_use_for_other_clients() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let (s1, s2) = {
        let mut a = Store::open(&path).await.unwrap().with_client_id("a");
        let s1 = a.create_session("a", None, None, None).await.unwrap();
        let s2 = a.create_session("b", None, None, None).await.unwrap();
        a.acquire_session_lock(s1, now_ms()).await.unwrap();
        (s1, s2)
    };

    let mut b = Store::open(&path).await.unwrap().with_client_id("b");
    b.acquire_session_lock(s2, now_ms()).await.unwrap();
    let list = b.list_sessions().await.unwrap();
    let in_use = |list: &[shuvarie_db::SessionSummary], id: uuid::Uuid| {
        list.iter().find(|s| s.id == id).unwrap().in_use
    };
    assert!(in_use(&list, s1), "other client's lock shows as in use");
    assert!(!in_use(&list, s2), "own lock never shows as in use");

    let mut anonymous = Store::open(&path).await.unwrap();
    let list = anonymous.list_sessions().await.unwrap();
    assert!(in_use(&list, s1));
    assert!(in_use(&list, s2));
}

#[tokio::test]
async fn delete_session_clears_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let id = {
        let mut a = Store::open(&path).await.unwrap().with_client_id("a");
        let id = a.create_session("doomed", None, None, None).await.unwrap();
        a.acquire_session_lock(id, now_ms()).await.unwrap();
        id
    };

    let mut b = Store::open(&path).await.unwrap().with_client_id("b");
    b.delete_session(id).await.unwrap();
    assert!(!b.locked_by_other(id, now_ms()).await.unwrap());
}

#[tokio::test]
async fn reopen_legacy_database_with_multiprocess_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    {
        let db = toasty::Db::builder()
            .models(toasty::models!(
                shuvarie_db::Session,
                shuvarie_db::Message,
                shuvarie_db::MessageEmbedding,
                shuvarie_db::ToolCall
            ))
            .build(toasty_driver_turso::Turso::file(&path).experimental_index_method(true))
            .await
            .unwrap();
        drop(db);
    }

    let mut store = Store::open(&path).await.unwrap();
    store
        .create_session("legacy", None, None, None)
        .await
        .unwrap();
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);
}

#[tokio::test]
async fn tree_messages_round_trip_with_parents_and_leaf() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("tree", None, None, None)
        .await
        .unwrap();

    let user = store
        .append_message(id, None, Role::User, "first")
        .await
        .unwrap();
    let assistant = store
        .append_assistant_message(
            id,
            Some(user.id),
            "reply",
            &[],
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    store.set_active_leaf(id, Some(assistant.id)).await.unwrap();

    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.leaf_id, Some(assistant.id));
    assert_eq!(loaded.messages[0].parent_id, None);
    assert_eq!(loaded.messages[1].parent_id, Some(user.id));
}

#[tokio::test]
async fn set_message_parent_reparents_and_persists() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("fork", None, None, None)
        .await
        .unwrap();
    let first = store
        .append_message(id, None, Role::User, "one")
        .await
        .unwrap();
    let second = store
        .append_message(id, None, Role::User, "two")
        .await
        .unwrap();
    let summary = store
        .append_summary(id, Some(first.id), "condensed")
        .await
        .unwrap();
    store
        .set_message_parent(second.id, Some(summary.id))
        .await
        .unwrap();

    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages[1].parent_id, Some(summary.id));
    assert!(loaded.messages[2].summary);
    assert_eq!(loaded.messages[0].parent_id, None);
}

#[tokio::test]
async fn delete_branch_removes_subtree_with_tool_calls() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("branch", None, None, None)
        .await
        .unwrap();
    let root = store
        .append_message(id, None, Role::User, "keep")
        .await
        .unwrap();
    let kept = store
        .append_assistant_message(
            id,
            Some(root.id),
            "kept reply",
            &[],
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    let forked = store
        .append_message(id, Some(root.id), Role::User, "forked away")
        .await
        .unwrap();
    let forked_assistant = store
        .append_assistant_message(
            id,
            Some(forked.id),
            "forked reply",
            &[],
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    store
        .append_tool_call(
            id,
            forked_assistant.id,
            0,
            "read_file",
            "{}",
            "out",
            "",
            true,
            false,
            None,
            "",
            None,
            None,
            0,
        )
        .await
        .unwrap();
    store.set_active_leaf(id, Some(kept.id)).await.unwrap();

    let removed = store.delete_branch(id, forked.id).await.unwrap();
    assert_eq!(removed, vec![forked.id, forked_assistant.id]);

    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.messages.len(), 2, "kept root + kept assistant");
    assert_eq!(loaded.leaf_id, Some(kept.id));
    assert!(loaded.tool_calls.is_empty(), "subtree tool calls removed");
}

#[tokio::test]
async fn set_active_leaf_is_clearable() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("leaf", None, None, None)
        .await
        .unwrap();
    let user = store
        .append_message(id, None, Role::User, "hi")
        .await
        .unwrap();
    store.set_active_leaf(id, Some(user.id)).await.unwrap();
    store.set_active_leaf(id, None).await.unwrap();
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(
        loaded.leaf_id,
        Some(shuvarie_db::EMPTY_LEAF),
        "a cleared leaf stores the EMPTY_LEAF sentinel"
    );
    let before = store.list_sessions().await.unwrap()[0].updated_at_epoch_ms;
    store.set_active_leaf(id, Some(user.id)).await.unwrap();
    assert_eq!(
        store.list_sessions().await.unwrap()[0].updated_at_epoch_ms,
        before,
        "leaf writes must not reorder the session list"
    );
}

#[tokio::test]
async fn appends_advance_the_active_leaf() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("leaf", None, None, None)
        .await
        .unwrap();

    let user = store
        .append_message(id, None, Role::User, "hi")
        .await
        .unwrap();
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.leaf_id, Some(user.id), "an append becomes the tip");

    store.set_active_leaf(id, None).await.unwrap();
    let assistant = store
        .append_assistant_message(
            id,
            Some(user.id),
            "hello",
            &[],
            &[],
            false,
            TokenUsage::default(),
            0.0,
            &TokenUsage::default(),
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    let summary = store
        .append_summary(id, Some(assistant.id), "so far")
        .await
        .unwrap();
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(
        loaded.leaf_id,
        Some(summary.id),
        "every append persists the new tip, even over a cleared leaf"
    );
}

#[tokio::test]
async fn set_scene_persists_and_clears() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("scenes", Some("ollama"), Some("m"), Some("Plan"))
        .await
        .unwrap();

    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.scene.as_deref(), Some("Plan"));

    store.set_scene(id, None).await.unwrap();
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(loaded.scene, None, "the built-in default stores NULL");
}
