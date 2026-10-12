# Images from the terminal: drops, pasted paths, and the clipboard

How a file reaches the composer's attachment pipeline from outside the app:
dragging an image onto the terminal, pasting a path, or pulling an image off
the system clipboard. Companion to [`attachments.md`](attachments.md), which
covers what happens to an attachment *after* it is in the composer.

## The transports

"Image pasting" is not one mechanism. Terminals deliver a dropped file in one
of three ways, none of them universal (and none guaranteed by any standard):

1. **Path as text.** The terminal types the file's path into the program, in
   practice as a single bracketed-paste payload (mode 2004, which we enable),
   so it arrives as one `Event::Paste`. This is the only transport we can rely
   on and the one everything below is built around. The observed encodings
   differ per terminal:

   | Terminal | Payload |
   | --- | --- |
   | kitty ≤ 0.46.2 (macOS) | `'/tmp/Screenshot 2026-05-27 at 13.37.31.png'` (single-quoted, spaces literal) |
   | Terminal.app, ghostty, wezterm (`quote_dropped_files="SpacesOnly"`) | `/tmp/Screenshot\ 2026-05-27\ at\ 13.37.31.png` (backslash-escaped) |
   | kitty 0.47.0 (macOS, regression) | `/tmp/Screenshot%202026-05-27%20at%2013.37.31.png` (percent-encoded) |
   | iTerm2, kitty (`text/uri-list`) | `file:///tmp/Screenshot%20….png` |
   | most others | a plain, unescaped path when nothing needs escaping |

   Caveat, and the reason the trace step exists: a kitty 0.47+ drop into a TUI
   that has not opted into the drag-and-drop protocol may deliver *nothing*
   at all (kitty #9895). Our fallback is not guaranteed to keep working.

2. **OSC 72 drag-and-drop.** kitty ≥ 0.47 and iTerm2 3.6 implement an opt-in
   handshake (`ESC ] 72 ; t=a ; <mime list> ST`), move/drop events carrying
   cell *and* pixel coordinates, and chunked (4096-byte, base64) transfer of
   the actual file bytes, plus symlink/directory handles, remote drops, and
   error responses. It would give real bytes with no escaping and a drop-target
   overlay — but the protocol is still marked under development (kitty RFC
   #9984), ghostty/wezterm only have feature requests, and **termina 0.3.3
   cannot see it**: `termina`'s `parse_osc` decodes only OSC 10–19 and silently
   discards every other OSC, so `ESC ] 72 …` never becomes an event. Supporting
   it means vendoring `termina` (or an upstream patch), which this scope
   declines. Consequently native DnD in kitty ≥ 0.47 is out of scope.

3. **OSC 5522 clipboard-image read.** kitty-only, gated behind
   `clipboard_control read-clipboard`, and not answered in terminfo.dev's
   feature matrix. Blocked by the same termina OSC filter. Out of scope; the
   clipboard is served by a subprocess instead (below).

## The directive escaping rule

A dropped path arrives as raw text and (today) becomes an `@path` directive in
the composer. The tokenizer ends a directive at the first whitespace, and
paths routinely contain spaces, so directives need an escaping rule. There is
now exactly one canonical encoding, in `src/tui/directive.rs`:

- a backslash before each whitespace character, and before each backslash:
  `/tmp/Screenshot\ 2026-05-27\ at\ 13.37.31.png`.

It is decoded by `at_directives` (send time) and by the `@` mention popup
(live completion), and it also fixes a pre-existing hole: before this, a typed
`@path with spaces` could not be a directive at all. Decoding is deliberately
conservative outside the escape set — a backslash that escapes neither
whitespace nor another backslash is kept literally, so hand-typed Windows
paths (`C:\Users\me\shot.png`) survive.

## What is built

- **Input trace (opt-in).** `SHUVARIE_TRACE_INPUT=1` (or a path) writes raw
  terminal input — paste payloads and key events, escaped — to
  `$TMPDIR/shuvarie-input-trace.log`. This exists so a user can confirm what
  *their* terminal actually sends on a drop before trusting the fallback. If a
  drop arrives as a burst of key events rather than one paste, the trace shows
  it.
- **Dropped-path paste → attachment.** `src/tui/paste_path.rs` classifies a
  paste payload: a single-line, path-shaped payload (absolute, `~/`, `./`,
  `../`, a Windows drive, or a separator plus a file-suffix-looking extension),
  after decoding `file://` URIs, surrounding quotes, backslash escapes, and
  percent-encoding, becomes a structural pending attachment. Anything else —
  prose, multi-line text — is ordinary text and pastes as before.
  Consequence worth knowing: pasting a *lone* path (copied from a terminal, say)
  attaches that file rather than inserting its text — the same classifier cannot
  tell a copied path from a drop, and the drop is the case that matters.
- **Structural pending attachments.** The composer holds a list of pending
  attachments rather than deriving them from the buffer text each frame. Typed
  `@path` directives still parse into the list (and remain in the prompt text);
  a dropped path lives only in the list. The attach strip renders both, and
  send carries every pending path.
- **Clipboard image paste.** A keybinding (`Ctrl+Shift+V`, or `Alt+Shift+V`)
  asks the core to read an image off the system clipboard: the configured
  `attachments { clipboard-command "…" }` program (or an auto-detected
  `wl-paste` / `xclip` / `pngpaste`), whose stdout bytes are sniffed, fitted to
  the image limits, and staged to a temp file that enters the same pending
  list. Stale staged files are swept on the next fetch.

## Known limitations

- **kitty ≥ 0.47 native DnD is unsupported** (OSC 72, termina filter), so a
  drop there may deliver nothing. Clipboard paste is the workaround.
- **OSC 5522 is unsupported** (same filter).
- **A drop of multiple files** (or a multi-word unquoted payload) is treated as
  one path; the probe reports the miss as an error chip.
- **Over SSH** the clipboard lives on the laptop, not the host running
  shuvarie; the user must supply a ferry command via `clipboard-command`.
- **Clipboard staging** writes a temp file (`$TMPDIR/shuvarie-paste-*/
  clipboard.png`) whose plain name becomes the attachment's display name; the
  whole directory is cleaned up by the next fetch's sweep, or by the OS. Feeding
  the bytes inline (no temp file) is a possible follow-up.
- The input trace and the temp staging are the only places raw payloads touch
  the filesystem; both are opt-in or short-lived.
