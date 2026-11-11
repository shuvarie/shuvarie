# Attachments & media

The attachment feature spans the stack around one currency type: `shuvarie-llm`'s `Attachment`
(metadata: kind/name/media_type/size/sha256) + `Blobs` (sha256 → bytes).

- `crates/doc/` (`shuvarie-doc`): pure (no process spawning) document → GFM-markdown
  conversion. `detect(bytes, ext)` (content signature first) + `to_markdown` + the
  `ConvertError` taxonomy (`NeedsOcr`/`Encrypted`/`Malformed`/`Unsupported`). Backends:
  delegated (`calamine`/`pdf-extract`/`rtf-parser`/`csv`) + hand-rolled zip+XML walkers
  (docx/pptx/odt/odp/epub).
- `crates/core/src/attachments.rs`: ingest (`resolve_attachments` — path read, sniff,
  image downscale/re-encode, document conversion) and request shaping (`prepare_for_send`
  — capability gate, text-only degradation, newest-first image budget), limits from the
  `attachments { … }` config via `AttachmentSettings`.
- `crates/core/src/attachments/picker.rs`: the `/attach` dialog's FFF index (one per
  searched directory, cached by the core task) and the ranking query; the walk runs on the
  blocking pool, each keystroke only re-ranks the in-memory snapshot. FFF's matcher drops
  fuzzy text below two bytes, so one-character queries are filtered by a substring scan
  here instead — the search starts at the first keystroke either way.
- `crates/core/src/attachments/compose.rs`: the shallow compose-side helpers — the strip's
  metadata-only probe and `directory_candidates`, the directory prompt's hints (one
  `read_dir`, subdirectories only, completed in the form being typed so `~`/relative/
  absolute input keeps its shape).
- `crates/core/src/office_convert.rs`: the optional external converter (`office-converter`
  config) for legacy `.doc`/`.ppt` — shared by the composer attachments and the file tools
- `crates/core/src/clipboard.rs`: the optional clipboard image reader (`clipboard-command`
  config, auto-detecting `wl-paste`/`xclip`/`pngpaste`/PowerShell) behind the composer's
  paste-from-clipboard keybinding — see [`images-and-drops.md`](images-and-drops.md)
- `crates/db/`: content-addressed `AttachmentBlob` store (sha256 → bytes, GC'd on delete
  cascades) + per-message `MessageAttachment` rows; session-file export/import carries base64 blobs.
- `src/tui/session/media.rs`: decode-once `MediaStore` (LRU-capped) + halfblock lifting
  through ratatui-image into ordinary `BodyChunk` lines; `blocks/media.rs` renders the
  per-turn attachment strip with hit regions; `viewer.rs` is the fullscreen gallery.
- `src/tui/attach.rs`: the centred `/attach [dir]` fuzzy-finder dialog (an `Overlay` whose
  pick lands in the composer's pending strip). Its search root is navigable without
  reopening the command — `→` re-roots onto the highlighted file's directory and `⌫` on an
  empty query walks up to the directory the dialog opened on — and each move re-asks the
  core for that tree's index. `paste_path.rs` is the drop-payload classifier (shape
  only: a paste becomes an attachment when the path names an existing regular file, a
  check `session.rs`'s `update` takes before the chip is pushed); `session.rs`
  holds the pending-attachment list — attachments are structural (picked, dropped, pasted,
  or read off the clipboard), never derived from the buffer text.
- `src/tui/manage_attachments.rs`: the `/manage-attachments` dialog, the same strip as one
  list to add to (Ctrl+A hands off to `attach.rs`, stacked over the dialog and returning to
  it with the pick appended), reorder (`Ctrl+↑↓`, which is send order), and remove (Delete).
  It owns only the cursor — the rows are `session.rs`'s pending attachments, so probes land
  on the chips it paints and every edit is already on the strip when it closes.
