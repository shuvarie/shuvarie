//! `message_attachments` + `attachment_blobs` integration: attach → load →
//! delete cascades, content-addressed blob survival, and the
//! interchange-format attachment round trip.

use base64::prelude::{BASE64_STANDARD, Engine as _};
use shuvarie_db::{FileAttachment, FileMessage, SessionFile, Store, StoredMessage, sha256_hex};
use shuvarie_llm::{Attachment, AttachmentKind, Role, TokenUsage};

async fn user_message(store: &mut Store, content: &str) -> (uuid::Uuid, StoredMessage) {
    let id = store
        .create_session("attachments", None, None, None)
        .await
        .unwrap();
    let message = store
        .append_message(id, None, Role::User, content)
        .await
        .unwrap();
    (id, message)
}

fn attachment(kind: AttachmentKind, name: &str, content: &[u8]) -> Attachment {
    Attachment {
        kind,
        name: name.to_string(),
        media_type: match kind {
            AttachmentKind::Image => "image/png",
            AttachmentKind::Document => "application/pdf",
        }
        .to_string(),
        size: content.len() as u64,
        sha256: sha256_hex(content),
    }
}

#[tokio::test]
async fn attach_round_trips_metadata_and_content_through_reload() {
    let mut store = Store::open_in_memory().await.unwrap();
    let (id, message) = user_message(&mut store, "what are these?").await;
    let image = b"\x89PNG image bytes".to_vec();
    let document = b"%PDF-1.4 document bytes".to_vec();
    let items = vec![
        (
            attachment(AttachmentKind::Image, "screenshot.png", &image),
            Some(image.clone()),
        ),
        (
            attachment(AttachmentKind::Document, "spec.pdf", &document),
            Some(document.clone()),
        ),
    ];
    store
        .attach_message_content(message.id, id, &items)
        .await
        .unwrap();

    let loaded = store.load_session(id).await.unwrap();
    let loaded_msg = &loaded.messages[0];
    let expected: Vec<Attachment> = items.iter().map(|(a, _)| a.clone()).collect();
    assert_eq!(loaded_msg.attachments, expected, "metadata in order");

    let blob = store
        .attachment_blob(&loaded_msg.attachments[0].sha256)
        .await
        .unwrap()
        .expect("image blob stored");
    assert_eq!(blob, image);
    let blob = store
        .attachment_blob(&loaded_msg.attachments[1].sha256)
        .await
        .unwrap()
        .expect("document blob stored");
    assert_eq!(blob, document);
}

#[tokio::test]
async fn blobs_survive_while_any_message_still_references_them() {
    let mut store = Store::open_in_memory().await.unwrap();
    let (id, first) = user_message(&mut store, "first").await;
    let second = store
        .append_message(id, Some(first.id), Role::User, "second")
        .await
        .unwrap();
    let image = b"identical bytes".to_vec();
    let item = (
        attachment(AttachmentKind::Image, "same.png", &image),
        Some(image.clone()),
    );
    let sha = item.0.sha256.clone();
    store
        .attach_message_content(first.id, id, std::slice::from_ref(&item))
        .await
        .unwrap();
    store
        .attach_message_content(second.id, id, &[item])
        .await
        .unwrap();
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(
        loaded.messages[0].attachments[0].sha256, loaded.messages[1].attachments[0].sha256,
        "same content → same address"
    );

    // Drop the second message: the first still references the blob.
    store.delete_message(second.id).await.unwrap();
    assert_eq!(
        store.attachment_blob(&sha).await.unwrap(),
        Some(image.clone()),
        "blob kept while referenced"
    );
    let loaded = store.load_session(id).await.unwrap();
    assert_eq!(
        loaded.messages[0].attachments.len(),
        1,
        "kept message's rows untouched"
    );

    // Drop the first message too: the blob is reclaimed.
    store.delete_message(first.id).await.unwrap();
    assert!(
        store.attachment_blob(&sha).await.unwrap().is_none(),
        "blob reclaimed with its last referencing message"
    );
}

#[tokio::test]
async fn attach_replaces_a_messages_previous_rows() {
    let mut store = Store::open_in_memory().await.unwrap();
    let (id, message) = user_message(&mut store, "swap").await;
    let first = b"first".to_vec();
    let second = b"second".to_vec();
    store
        .attach_message_content(
            message.id,
            id,
            &[(
                attachment(AttachmentKind::Image, "a.png", &first),
                Some(first),
            )],
        )
        .await
        .unwrap();
    store
        .attach_message_content(
            message.id,
            id,
            &[(
                attachment(AttachmentKind::Image, "b.png", &second),
                Some(second),
            )],
        )
        .await
        .unwrap();

    let loaded = store.load_session(id).await.unwrap();
    let atts = &loaded.messages[0].attachments;
    assert_eq!(atts.len(), 1, "old rows replaced");
    assert_eq!(atts[0].name, "b.png");
}

#[tokio::test]
async fn deleting_a_branch_gcs_its_unique_blob() {
    let mut store = Store::open_in_memory().await.unwrap();
    let (id, message) = user_message(&mut store, "gone soon").await;
    let image = b"unique bytes".to_vec();
    let item = (
        attachment(AttachmentKind::Image, "doomed.png", &image),
        Some(image.clone()),
    );
    let sha = item.0.sha256.clone();
    store
        .attach_message_content(message.id, id, &[item])
        .await
        .unwrap();
    assert!(store.attachment_blob(&sha).await.unwrap().is_some());

    store.delete_branch(id, message.id).await.unwrap();
    assert!(
        store.attachment_blob(&sha).await.unwrap().is_none(),
        "blob reclaimed with its branch"
    );
}

/// Interchange format: an exported file hydrates blobs, an import restores
/// them, and a metadata-only record imports without a dangling blob.
#[tokio::test]
async fn export_import_round_trips_attachments() {
    let mut source = Store::open_in_memory().await.unwrap();
    let (id, message) = user_message(&mut source, "export me").await;
    let image = b"exported image bytes".to_vec();
    let document = b"exported doc bytes".to_vec();
    source
        .attach_message_content(
            message.id,
            id,
            &[
                (
                    attachment(AttachmentKind::Image, "photo.png", &image),
                    Some(image.clone()),
                ),
                (
                    attachment(AttachmentKind::Document, "readme.pdf", &document),
                    Some(document.clone()),
                ),
            ],
        )
        .await
        .unwrap();

    let stored = source.load_session(id).await.unwrap();
    let mut file = SessionFile::from_stored(&stored);
    source.hydrate_attachment_blobs(&mut file).await.unwrap();
    file.to_json().unwrap(); // serializable

    let mut target = Store::open_in_memory().await.unwrap();
    let imported = target.import_session(&file).await.unwrap();
    let loaded = target.load_session(imported).await.unwrap();
    let atts = &loaded.messages[0].attachments;
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0].name, "photo.png");
    assert_eq!(atts[0].kind, AttachmentKind::Image);
    assert_eq!(atts[1].name, "readme.pdf");
    assert_eq!(
        target.attachment_blob(&atts[0].sha256).await.unwrap(),
        Some(image)
    );
    assert_eq!(
        target.attachment_blob(&atts[1].sha256).await.unwrap(),
        Some(document)
    );
}

#[tokio::test]
async fn metadata_only_import_attaches_without_a_blob() {
    let mut store = Store::open_in_memory().await.unwrap();
    let file = SessionFile {
        format: shuvarie_db::session_file::FILE_FORMAT,
        session: shuvarie_db::FileSession {
            id: uuid::Uuid::now_v7(),
            title: "metadata only".to_string(),
            provider: None,
            model: None,
            scene: None,
            leaf_id: Some(1),
            created_at: None,
            updated_at: None,
        },
        messages: vec![FileMessage {
            id: 1,
            parent_id: None,
            seq: 0,
            role: shuvarie_db::MsgRole::User,
            content: "without bytes".to_string(),
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
            model_code: None,
            scene: None,
            duration_ms: None,
            attachments: vec![FileAttachment {
                seq: 0,
                kind: AttachmentKind::Image,
                name: "lost.png".to_string(),
                media_type: "image/png".to_string(),
                size: 99,
                sha256: "ab".repeat(32),
                content_base64: None,
            }],
        }],
        tool_calls: Vec::new(),
        scroll: shuvarie_db::StoredScroll::default(),
    };

    let imported = store.import_session(&file).await.unwrap();
    let loaded = store.load_session(imported).await.unwrap();
    let atts = &loaded.messages[0].attachments;
    assert_eq!(atts.len(), 1, "metadata attached");
    assert!(
        store
            .attachment_blob(&atts[0].sha256)
            .await
            .unwrap()
            .is_none()
    );
}

/// A content/hash mismatch (corrupt or tampered export) must not poison the
/// blob store: that attachment imports metadata-only, and a well-formed
/// sibling still gets its bytes.
#[tokio::test]
async fn import_drops_blob_content_that_fails_the_hash_check() {
    let mut store = Store::open_in_memory().await.unwrap();
    let file = SessionFile {
        format: shuvarie_db::session_file::FILE_FORMAT,
        session: shuvarie_db::FileSession {
            id: uuid::Uuid::now_v7(),
            title: "corrupt".to_string(),
            provider: None,
            model: None,
            scene: None,
            leaf_id: Some(1),
            created_at: None,
            updated_at: None,
        },
        messages: vec![FileMessage {
            id: 1,
            parent_id: None,
            seq: 0,
            role: shuvarie_db::MsgRole::User,
            content: "corrupt".to_string(),
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
            model_code: None,
            scene: None,
            duration_ms: None,
            attachments: vec![
                FileAttachment {
                    seq: 0,
                    kind: AttachmentKind::Image,
                    name: "corrupt.png".to_string(),
                    media_type: "image/png".to_string(),
                    size: 3,
                    sha256: sha256_hex(b"correct bytes"),
                    content_base64: Some(BASE64_STANDARD.encode(b"wrong bytes")),
                },
                FileAttachment {
                    seq: 1,
                    kind: AttachmentKind::Document,
                    name: "good.pdf".to_string(),
                    media_type: "application/pdf".to_string(),
                    size: 7,
                    sha256: sha256_hex(b"goodies"),
                    content_base64: Some(BASE64_STANDARD.encode(b"goodies")),
                },
            ],
        }],
        tool_calls: Vec::new(),
        scroll: shuvarie_db::StoredScroll::default(),
    };

    let imported = store.import_session(&file).await.unwrap();
    let loaded = store.load_session(imported).await.unwrap();
    let atts = &loaded.messages[0].attachments;
    assert!(
        store
            .attachment_blob(&atts[0].sha256)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.attachment_blob(&atts[1].sha256).await.unwrap(),
        Some(b"goodies".to_vec())
    );
}
