use shuvarie_db::{
    DbError, FileMessage, ReasoningSegment, SessionFile, Store, StoredScroll, TextSegment,
};
use shuvarie_llm::{Role, TokenUsage};

async fn build_source_session(store: &mut Store) -> shuvarie_db::StoredSession {
    let id = store
        .create_session("exported", Some("ollama"), Some("model-x"), Some("Plan"))
        .await
        .unwrap();
    let user = store
        .append_message(id, None, Role::User, "please look")
        .await
        .unwrap();
    let assistant = store
        .append_assistant_message(
            id,
            Some(user.id),
            "looked at it\n\nagain",
            &[
                ReasoningSegment {
                    after_tool: 0,
                    text: "thinking".to_string(),
                    duration_ms: 120,
                },
                ReasoningSegment {
                    after_tool: 1,
                    text: "more".to_string(),
                    duration_ms: 5,
                },
            ],
            &[
                TextSegment {
                    after_tool: 0,
                    text: "looked at it".to_string(),
                },
                TextSegment {
                    after_tool: 1,
                    text: "\n\nagain".to_string(),
                },
            ],
            false,
            TokenUsage {
                input_tokens: 12_000,
                output_tokens: 300,
                total_tokens: 12_300,
                cached_input_tokens: 11_000,
                ..TokenUsage::default()
            },
            0.25,
            &TokenUsage {
                input_tokens: 12_000,
                output_tokens: 300,
                total_tokens: 12_300,
                cached_input_tokens: 11_000,
                ..TokenUsage::default()
            },
            &shuvarie_db::Attribution::default(),
        )
        .await
        .unwrap();
    store
        .append_tool_call(
            id,
            assistant.id,
            0,
            "read_file",
            r#"{"path":"a.txt"}"#,
            "contents",
            "",
            true,
            false,
            None,
            "",
            None,
            None,
            42,
        )
        .await
        .unwrap();
    store.set_active_leaf(id, Some(assistant.id)).await.unwrap();
    store
        .set_scroll(
            id,
            StoredScroll {
                sticky: false,
                anchor: Some((1, 7)),
            },
        )
        .await
        .unwrap();
    store.load_session(id).await.unwrap()
}

#[tokio::test]
async fn export_import_round_trips_between_stores() {
    let mut source = Store::open_in_memory().await.unwrap();
    let stored = build_source_session(&mut source).await;
    let file = SessionFile::from_stored(&stored);

    let mut target = Store::open_in_memory().await.unwrap();
    let imported_id = target.import_session(&file).await.unwrap();

    let loaded = target.load_session(imported_id).await.unwrap();
    assert_eq!(loaded.id, file.session.id, "id kept when free");
    assert_eq!(loaded.title, "exported");
    assert_eq!(loaded.provider.as_deref(), Some("ollama"));
    assert_eq!(loaded.model.as_deref(), Some("model-x"));
    assert_eq!(loaded.scene.as_deref(), Some("Plan"));
    assert_eq!(
        loaded.created_at.as_millisecond(),
        stored.created_at.as_millisecond(),
        "timestamps carry millisecond precision"
    );
    assert_eq!(
        loaded.updated_at.as_millisecond(),
        stored.updated_at.as_millisecond()
    );
    assert_eq!(loaded.scroll, stored.scroll);

    assert_eq!(loaded.messages.len(), 2);
    assert_eq!(loaded.messages[0].role, stored.messages[0].role);
    assert_eq!(loaded.messages[0].content, "please look");
    assert_eq!(loaded.messages[0].seq, stored.messages[0].seq);
    assert_eq!(loaded.messages[1].reasoning, stored.messages[1].reasoning);
    assert_eq!(
        loaded.messages[1].text_segments,
        stored.messages[1].text_segments
    );
    assert_eq!(loaded.messages[1].input_tokens, 12_000);
    assert_eq!(loaded.messages[1].cached_input_tokens, 11_000);
    assert_eq!(loaded.messages[1].reasoning_tokens, 0);
    assert!((loaded.messages[1].cost - 0.25).abs() < f64::EPSILON);
    assert_eq!(loaded.messages[1].request, stored.messages[1].request);

    assert_eq!(
        loaded.tool_calls.len(),
        1,
        "tool call carried over with a remapped message id"
    );
    assert_eq!(loaded.tool_calls[0].name, "read_file");
    assert_eq!(loaded.tool_calls[0].args_json, r#"{"path":"a.txt"}"#);
    assert_eq!(loaded.tool_calls[0].output, "contents");
    assert_eq!(loaded.tool_calls[0].duration_ms, 42);
    assert_eq!(loaded.tool_calls[0].message_id, loaded.messages[1].id);
}

#[tokio::test]
async fn export_import_round_trips_the_active_path() {
    let mut source = Store::open_in_memory().await.unwrap();
    let stored = build_source_session(&mut source).await;
    let file = SessionFile::from_stored(&stored);

    let mut target = Store::open_in_memory().await.unwrap();
    let id = target.import_session(&file).await.unwrap();
    let loaded = target.load_session(id).await.unwrap();

    assert_eq!(loaded.leaf_id, Some(loaded.messages[1].id), "leaf remapped");
    assert_eq!(loaded.messages[1].parent_id, Some(loaded.messages[0].id));
}

#[tokio::test]
async fn import_into_the_same_store_mints_a_new_session_id() {
    let mut store = Store::open_in_memory().await.unwrap();
    let stored = build_source_session(&mut store).await;
    let file = SessionFile::from_stored(&stored);

    let reimported = store.import_session(&file).await.unwrap();
    assert_ne!(reimported, stored.id);

    let list = store.list_sessions().await.unwrap();
    assert_eq!(list.len(), 2);
    let loaded = store.load_session(reimported).await.unwrap();
    assert_eq!(loaded.messages.len(), 2);
    assert_eq!(loaded.leaf_id, Some(loaded.messages[1].id));
    assert_ne!(
        loaded.messages[1].id, stored.messages[1].id,
        "message ids are regenerated"
    );
}

#[tokio::test]
async fn import_of_a_file_without_a_leaf_falls_back_to_none() {
    let mut store = Store::open_in_memory().await.unwrap();
    let file = SessionFile {
        format: shuvarie_db::session_file::FILE_FORMAT,
        session: shuvarie_db::FileSession {
            id: uuid::Uuid::now_v7(),
            title: "empty".to_string(),
            provider: None,
            model: None,
            scene: None,
            leaf_id: None,
            created_at: None,
            updated_at: None,
        },
        messages: vec![FileMessage {
            id: 1,
            parent_id: None,
            seq: 0,
            role: shuvarie_db::MsgRole::User,
            content: "solo".to_string(),
            reasoning: Vec::new(),
            text_segments: Vec::new(),
            interrupted: false,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: false,
            request: TokenUsage::default(),
        }],
        tool_calls: Vec::new(),
        scroll: StoredScroll::default(),
    };

    let id = store.import_session(&file).await.unwrap();
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(
        loaded.leaf_id, None,
        "the stored leaf stays unset; the reload path falls back to the newest message"
    );
}

#[tokio::test]
async fn export_import_round_trips_a_cleared_leaf() {
    let mut source = Store::open_in_memory().await.unwrap();
    let id = source
        .create_session("cleared", None, None, None)
        .await
        .unwrap();
    source
        .append_message(id, None, Role::User, "one")
        .await
        .unwrap();
    source.set_active_leaf(id, None).await.unwrap();
    let stored = source.load_session(id).await.unwrap();
    assert_eq!(stored.leaf_id, Some(shuvarie_db::EMPTY_LEAF));
    let file = SessionFile::from_stored(&stored);

    let mut target = Store::open_in_memory().await.unwrap();
    let imported = target.import_session(&file).await.unwrap();
    let loaded = target.load_session(imported).await.unwrap();
    assert_eq!(
        loaded.leaf_id,
        Some(shuvarie_db::EMPTY_LEAF),
        "the empty active path survives the round trip"
    );
}

#[tokio::test]
async fn failed_import_cleans_up_the_partial_rows() {
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("live", None, None, None)
        .await
        .unwrap();
    store
        .append_message(id, None, Role::User, "x")
        .await
        .unwrap();

    let mut file = SessionFile::from_stored(&store.load_session(id).await.unwrap());
    // A NaN cost cannot be stored (SQLite folds NaN to NULL, which violates
    // the NOT NULL constraint), so the first message insert fails mid-import.
    file.messages[0].cost = f64::NAN;

    let err = store.import_session(&file).await.unwrap_err();
    assert!(matches!(err, DbError::Query(_)), "unexpected error: {err}");
    assert_eq!(
        store.list_sessions().await.unwrap().len(),
        1,
        "the half-imported session row was removed"
    );
    let live = store.load_session(id).await.unwrap();
    assert_eq!(live.messages.len(), 1, "the source session is intact");
}
