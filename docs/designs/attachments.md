# Attachments & media

The attachment feature spans the stack around one currency type: `shuvarie-llm`'s `Attachment`
(metadata: kind/name/media_type/size/sha256) + `Blobs` (sha256 → bytes).

- `crates/doc/` (`shuvarie-doc`): pure (no process spawning) document → GFM-markdown
  conversion. `detect(bytes, ext)` (content signature first) + `to_markdown` + the
  `ConvertError` taxonomy (`NeedsOcr`/`Encrypted`/`Malformed`/`Unsupported`). Backends:
  delegated (`calamine`/`pdf-extract`/`rtf-parser`/`csv`) + hand-rolled zip+XML walkers
  (docx/pptx/odt/odp/epub).
- `crates/core/src/attachments.rs`: ingest (`resolve_directives` — @path read, sniff,
  image downscale/re-encode, document conversion) and request shaping (`prepare_for_send`
  — capability gate, text-only degradation, newest-first image budget), limits from the
  `attachments { … }` config via `AttachmentSettings`.
- `crates/core/src/office_convert.rs`: the optional external converter (`office-converter`
  config) for legacy `.doc`/`.ppt` — shared by the composer attachments and the file tools
- `crates/db/`: content-addressed `AttachmentBlob` store (sha256 → bytes, GC'd on delete
  cascades) + per-message `MessageAttachment` rows; session-file export/import carries base64 blobs.
- `src/tui/session/media.rs`: decode-once `MediaStore` (LRU-capped) + halfblock lifting
  through ratatui-image into ordinary `BodyChunk` lines; `blocks/media.rs` renders the
  per-turn attachment strip with hit regions; `viewer.rs` is the fullscreen gallery;
  `session/mention.rs` the compose-time preview/completion.
