# Onboarding (the welcome screen)

Onboarding is the flow a run with no connected provider starts in. It replaces the session
instead of floating over it, so there is no way into an unconfigured session — the only exit is
a configured provider.

- `src/tui/welcome.rs`: the page machine. `WelcomeStage` is `Intro → AddProvider → ModelPicker →
  AllSet`; `Welcome::{open,close}` raise and drop the flow, `map_event` maps Ctrl+C to
  `RequestQuit` on every page and Esc to it on the two interstitial ones (the form and the picker
  have a `Cancel`/accept meaning for Esc, so it is left to them), and any other key press to
  `Advance` on the interstitial pages,
  and `update` turns `Advance` into `AddProvider` (from the title) or `Finish` (from the closing
  page). `view` paints the interstitial pages only; the form and the picker paint themselves.
- `src/tui/welcome/art.rs`: the title page's drawing. The `Shuvarie` wordmark is a `tui-big-text`
  `BigText` line at `PixelSize::Full`, so each letter is one 8x8 `font8x8` glyph box and the box the
  layout reserves is eight cells per letter; `the_wordmark_paints_exactly_its_box` holds the widget
  to it. The rook is the `shuvarie-artwork` repository's `png/rook.png`, vendored into `assets/` and
  embedded with `include_bytes!`, decoded once into a coverage `Mask` trimmed to the pixels that
  carry ink, and painted as half blocks (`▀`/`▄`/`█`) in the dim tone: a cell its silhouette does not
  reach is left alone, so the rain keeps falling behind it. Its box is derived from the viewport
  alone — `ROOK_HEIGHT_PERCENT` of the rows, standing on the bottom row with `ROOK_CROP_PERCENT` of
  its width running off the right edge, and the width carrying the artwork's aspect over the half
  blocks' grain of two sub-pixel rows per cell — and it is dropped entire when the viewport is too
  short or too narrow for the part that shows. The artwork is sampled across its *whole* width, the
  cropped columns included, so the viewport cuts the silhouette off rather than squeezing it into
  the room left over. The arrow rain falls on the diagonal: every lane enters on the top row or the
  left column (`lanes`), so between them they head every diagonal the page has and no two streaks
  can share a cell. Each drop crosses its own diagonal at one cell per row (`DRIFT`), painting a
  `\` shaft that trails `TRAIL` cells back up and to the left of its bright tip; period and phase
  come from a hash of the entry cell, so it needs no state: `view` paints whatever frame the
  `FrameIndexer` clock selects and `next_wake` reports when the loop should come back.
- `src/tui/app.rs`: `welcome.open` is the single "onboarding is running" flag. `App::view` skips
  every session widget while it is set, `welcome_overlay(stage)` maps the stage onto the overlay
  that routes that page's keys, pastes, and registry events (`Welcome` for the interstitial pages,
  `AddProvider`/`ModelPicker` for the dialogs), and `set_welcome_stage` moves both together. The
  flow's transitions: form `Submit` sends `AddProvider`/`SetActiveProvider`/`ListModels` and stays
  on the form; `ModelsLoaded` pages on to the picker (or straight to `AllSet` when the provider
  lists nothing) and `ModelsError` returns to the form with the error; the picker's `Selected` and
  Esc-with-a-default land on `AllSet`; `AllSet`'s next key press finishes onboarding. Esc on the
  form's first page walks back to the title. Cancelling the quit prompt calls `close_overlay`,
  which re-points the overlay at the stage still showing, so it lands back on the page it was
  asked from.
- `src/tui/add_provider.rs` / `src/tui/model_picker.rs`: `dialog_rect(area, fullscreen, …)` picks
  between the centred popup and `fullscreen_rect`, the viewport inset by a small margin; the
  render loop's `set_viewport` geometry mirrors the same choice through
  `add_provider_list_height`/`model_picker_list_height`.
- `src/tui.rs`: the render loop wakes at the earliest of the spinners' next frame and the welcome
  art's (`App::welcome.animation_wake`), both wall-clock driven, so the title animates without any
  render-loop state.
