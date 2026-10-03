//! Request shaping: the prompt's and history's attachments are fitted to the
//! streaming target just before a turn sends. A capability failure on *new*
//! images aborts the turn; old images on a text-only model degrade to noted
//! text; the image payload budgets newest-first across the whole request.
//! Persisted content preloads for the request too.

use shuvarie_llm::{Attachment, AttachmentKind, ChatMsg};

#[cfg(test)]
use super::sha256_hex;
use super::{AttachmentSettings, Prepared};

/// Whether the streaming target accepts image parts: the catalog's
/// `attachment` flag wins when it knows the model; without a catalog entry
/// the transport decides (rig's OpenAI-family, Anthropic, Gemini, and Ollama
/// clients render image parts; Copilot, Cohere, and Voyage do not).
pub fn supports_images(
    provider_type: selune::ProviderType,
    catalog_model: Option<&selune::Model>,
) -> bool {
    use selune::ProviderType::*;
    if let Some(model) = catalog_model {
        return model.attachment;
    }
    matches!(
        provider_type,
        Openai
            | OpenaiCompat
            | Openrouter
            | Vercel
            | Anthropic
            | Google
            | Azure
            | Bedrock
            | GoogleVertex
            | Ollama
            | Chatgpt
            | Llamafile
    )
}

/// Fit a whole request (history + prompt attachments) to the streaming
/// target, mutating in place:
///
/// 1. `Err` when the prompt carries images the model cannot receive — the
///    caller aborts the turn (history images instead degrade per 2).
/// 2. Removes history images for a non-image target, leaving a content note
///    behind so the model still sees that the user showed it something.
/// 3. Budgets image bytes across the request newest-first (the prompt is
///    newest and never trimmed), removing history images beyond
///    `settings.image_budget`.
///
/// Returns the accommodations it made so the caller can surface them to the
/// user (they are otherwise silent in the request).
pub fn prepare_for_send(
    history: &mut [ChatMsg],
    prompt_attachments: &[Attachment],
    model: &str,
    supports_images: bool,
    settings: &AttachmentSettings,
) -> Result<FitReport, String> {
    let mut report = FitReport::default();
    let prompt_images: Vec<&Attachment> = prompt_attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::Image)
        .collect();
    let prompt_bytes: u64 = prompt_images
        .iter()
        .map(|a| a.size)
        .fold(0u64, u64::saturating_add);
    if !supports_images {
        if !prompt_images.is_empty() {
            return Err(format!(
                "model {model:?} does not support image attachments — pick a multimodal model \
                 (or remove the images)"
            ));
        }
        for msg in history.iter_mut() {
            if msg.attachments.is_empty() {
                continue;
            }
            report.degraded_history += strip_images(msg, "current model has no image support");
        }
        return Ok(report);
    }
    // Newest-first budget walk: images beyond the cap drop from the oldest
    // messages, prompt images never trim.
    let mut remaining = settings.image_budget.saturating_sub(prompt_bytes);
    for msg in history.iter_mut().rev() {
        let has_image = msg
            .attachments
            .iter()
            .any(|a| a.kind == AttachmentKind::Image);
        if !has_image {
            continue;
        }
        let mut notes: Vec<String> = Vec::new();
        msg.attachments.retain(|a| {
            if a.kind != AttachmentKind::Image {
                return true;
            }
            if a.size <= remaining {
                remaining -= a.size;
                true
            } else {
                notes.push(drop_note(a, "trimmed to fit the image budget"));
                false
            }
        });
        if !notes.is_empty() {
            report.trimmed_history += notes.len();
            let mut content = std::mem::take(&mut msg.content);
            for note in notes {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&note);
            }
            msg.content = content;
        }
    }
    Ok(report)
}

/// The request-side note a removed attachment leaves behind.
fn drop_note(attachment: &Attachment, reason: &str) -> String {
    format!("[{} {:?} — {reason}]", attachment.kind, attachment.name)
}

/// Remove a message's images, leaving one note per image behind so the
/// model still knows what it loses access to. Returns the removed count.
fn strip_images(msg: &mut ChatMsg, reason: &str) -> usize {
    let names: Vec<String> = msg
        .attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::Image)
        .map(|a| drop_note(a, reason))
        .collect();
    let removed = names.len();
    if removed == 0 {
        return 0;
    }
    msg.attachments.retain(|a| a.kind != AttachmentKind::Image);
    let mut content = std::mem::take(&mut msg.content);
    for note in names {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&note);
    }
    msg.content = content;
    removed
}

/// A stored history attachment as a re-sendable prepared attachment:
/// metadata kept, bytes looked up from the blob store when they survive
/// (missing content degrades to a note in the request).
pub async fn resolve_stored_prepared(
    store: &mut shuvarie_db::Store,
    metas: Vec<Attachment>,
) -> Vec<Prepared> {
    let mut prepared = Vec::with_capacity(metas.len());
    for meta in metas {
        let bytes = store.attachment_blob(&meta.sha256).await.ok().flatten();
        prepared.push(Prepared { meta, bytes });
    }
    prepared
}

/// Preload every attachment's persisted content for one request: the
/// history's and the prompt's. Content-addressed blobs load per sha256; a
/// missing or failed blob simply leaves the key out (the renderer degrades
/// that attachment to a note).
pub async fn collect_blobs(
    store: &mut shuvarie_db::Store,
    history: &[ChatMsg],
    prompt_attachments: &[Attachment],
) -> shuvarie_llm::Blobs {
    let mut shas: Vec<String> = history
        .iter()
        .flat_map(|msg| msg.attachments.iter().map(|a| a.sha256.clone()))
        .chain(prompt_attachments.iter().map(|a| a.sha256.clone()))
        .collect();
    shas.sort();
    shas.dedup();
    let mut blobs = shuvarie_llm::Blobs::new();
    for sha in shas {
        if let Ok(Some(content)) = store.attachment_blob(&sha).await {
            blobs.insert(sha, content);
        }
    }
    blobs
}

/// What [`prepare_for_send`] had to accommodate in the request, for the
/// TUI's status row (the accommodations are silent in the request itself).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FitReport {
    /// History images removed to fit the image budget.
    pub trimmed_history: usize,
    /// History images degraded to notes on a non-image target.
    pub degraded_history: usize,
}

impl FitReport {
    /// Whether anything moved (the notice only shows for real changes).
    pub fn any(&self) -> bool {
        self.trimmed_history > 0 || self.degraded_history > 0
    }

    /// The status-row text for the report, `None` when nothing changed.
    pub fn notice_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.trimmed_history > 0 {
            parts.push(format!(
                "{} history image{} trimmed to fit the image budget",
                self.trimmed_history,
                if self.trimmed_history == 1 { "" } else { "s" }
            ));
        }
        if self.degraded_history > 0 {
            parts.push(format!(
                "{} history image{} degraded to notes (current model has no image support)",
                self.degraded_history,
                if self.degraded_history == 1 { "" } else { "s" }
            ));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_meta(name: &str, size: u64) -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            name: name.to_string(),
            media_type: "image/png".to_string(),
            size,
            sha256: name.to_string(),
        }
    }

    fn plain_model(id: &str) -> selune::Model {
        let (org, model) = id.split_once('/').unwrap_or(("test-org", id));
        selune::Model {
            id: id.to_string(),
            model_code: selune::ModelCode::new(org, model, None).expect("model code"),
            name: id.to_string(),
            reasoning: false,
            reasoning_options: Vec::new(),
            attachment: false,
            limit: selune::ModelLimit::default(),
            cost: selune::ModelCost::default(),
            options: None,
        }
    }

    fn user_msg(content: &str, attachments: Vec<Attachment>) -> ChatMsg {
        let mut msg = ChatMsg::user(content);
        msg.attachments = attachments;
        msg
    }

    #[test]
    fn the_catalog_attachment_flag_decides_transport_defaults_last() {
        use selune::ProviderType;
        let mut flag_true = plain_model("m");
        flag_true.attachment = true;
        assert!(supports_images(ProviderType::Copilot, Some(&flag_true)));
        assert!(!supports_images(
            ProviderType::Openai,
            Some(&plain_model("m"))
        ));
        assert!(supports_images(ProviderType::Openai, None));
        assert!(supports_images(ProviderType::Anthropic, None));
        assert!(supports_images(ProviderType::Ollama, None));
        assert!(!supports_images(ProviderType::Copilot, None));
        assert!(!supports_images(ProviderType::Cohere, None));
        assert!(!supports_images(ProviderType::Voyageai, None));
    }

    #[test]
    fn prompt_images_on_a_text_only_model_abort_the_turn() {
        let mut history: Vec<ChatMsg> = Vec::new();
        let prompt = [image_meta("shot.png", 12)];
        let error = prepare_for_send(
            &mut history,
            &prompt,
            "old-text-model",
            false,
            &AttachmentSettings::default(),
        )
        .unwrap_err();
        assert!(
            error.contains("does not support image attachments"),
            "{error}"
        );
    }

    #[test]
    fn history_images_on_a_text_model_degrade_to_notes() {
        let mut history = vec![user_msg("look", vec![image_meta("shot.png", 12)])];
        prepare_for_send(
            &mut history,
            &[],
            "old-text-model",
            false,
            &AttachmentSettings::default(),
        )
        .unwrap();
        assert!(history[0].attachments.is_empty(), "images stripped");
        assert!(
            history[0]
                .content
                .contains("[image \"shot.png\" \u{2014} current model has no image support]"),
            "{}",
            history[0].content
        );
    }

    #[test]
    fn the_request_image_budget_trims_oldest_first_and_never_the_prompt() {
        let meg = 1024 * 1024;
        let mut history = vec![
            user_msg("a", vec![image_meta("old.png", 8 * meg as u64)]),
            user_msg("b", vec![image_meta("mid.png", 8 * meg as u64)]),
            user_msg("c", vec![image_meta("new.png", 8 * meg as u64)]),
        ];
        let prompt = [image_meta("prompt.png", 4 * meg as u64)];
        let settings = AttachmentSettings {
            image_budget: 12 * meg as u64,
            ..AttachmentSettings::default()
        };
        prepare_for_send(&mut history, &prompt, "vision", true, &settings).unwrap();
        assert_eq!(history[2].attachments.len(), 1, "newest kept");
        assert!(history[1].attachments.is_empty(), "over-budget dropped");
        assert!(history[0].attachments.is_empty(), "over-budget dropped");
        assert!(
            history[1]
                .content
                .contains("[image \"mid.png\" \u{2014} trimmed to fit the image budget]"),
            "{}",
            history[1].content
        );
        assert_eq!(prompt[0].size, 4 * meg as u64, "prompt images never trim");
    }

    #[test]
    fn documents_ride_regardless_of_image_capability() {
        let document = Attachment {
            kind: AttachmentKind::Document,
            name: "spec.pdf".into(),
            media_type: "application/pdf".into(),
            size: 700,
            sha256: "cd".into(),
        };
        let mut history = Vec::new();
        prepare_for_send(
            &mut history,
            std::slice::from_ref(&document),
            "old-text-model",
            false,
            &AttachmentSettings::default(),
        )
        .expect("a converted document is text and needs no image support");
    }

    #[tokio::test]
    async fn collect_blobs_round_trips_the_persisted_content() {
        let mut store = shuvarie_db::Store::open_in_memory().await.unwrap();
        let id = store.create_session("t", None, None, None).await.unwrap();
        let message = store
            .append_message(id, None, shuvarie_llm::Role::User, "p")
            .await
            .unwrap();
        let content = b"png bytes".to_vec();
        let meta = Attachment {
            sha256: sha256_hex(&content),
            ..image_meta("shot.png", content.len() as u64)
        };
        store
            .attach_message_content(message.id, id, &[(meta.clone(), Some(content.clone()))])
            .await
            .unwrap();
        let blobs = collect_blobs(&mut store, &[], &[meta]).await;
        let expected_key = sha256_hex(b"png bytes");
        assert_eq!(
            blobs.get(&expected_key).map(Vec::as_slice),
            Some(b"png bytes".as_slice()),
            "the persisted content loads by sha"
        );
        // A sha with no blob simply stays out of the map.
        let blobs = collect_blobs(
            &mut store,
            &[],
            &[
                image_meta("lost.png", 1), /* sha256 = "lost.png" — not a store key */
            ],
        )
        .await;
        assert!(!blobs.contains_key("lost.png"));
    }

    #[test]
    fn fit_report_notice_covers_both_accommodations() {
        let mut report = FitReport::default();
        assert!(report.notice_text().is_none());
        report.trimmed_history = 1;
        assert_eq!(
            report.notice_text().as_deref(),
            Some("1 history image trimmed to fit the image budget")
        );
        report.degraded_history = 2;
        assert_eq!(
            report.notice_text().as_deref(),
            Some(
                "1 history image trimmed to fit the image budget, 2 history images degraded to \
                 notes (current model has no image support)"
            )
        );
    }
}
