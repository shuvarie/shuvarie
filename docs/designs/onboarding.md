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
  to it. The rook is the `shuvarie-artwork` repository's `png/rook.png` — the pixel grid that PNG
  scales up five times over, cropped to the pixels that carry ink — embedded as the grid itself
  (`ROOK_ART`, `#` inked and `.` clear) rather than as the image, so no decoder runs and the
  silhouette can be read straight off the source. It is painted as half blocks (`▀`/`▄`/`█`) in the
  dim tone, one art pixel per cell half: a cell is one art column wide and two art rows tall, read
  off the grid with no resampling, and a cell its silhouette does not reach is left alone so the
  rain keeps falling behind it. The rook is therefore a **fixed size** (`ROOK_COLUMNS` x `ROOK_ROWS`,
  the artwork's own pixels), which the terminal never scales: its box stands on the bottom row with
  `ROOK_CROP_PERCENT` of its width running off the right edge, so the viewport cuts the silhouette
  rather than squeezing it into the room left over. What a cramped viewport cannot hold is clipped
  off the silhouette rather than dropping it: a viewport shorter than the rook takes its top and one
  narrower than the part that shows takes its left, and the cells that are left stay pinned to the
  corner (`rook_rect`, `a_cramped_viewport_clips_the_rook_rather_than_scaling_it`). The arrow rain
  falls on the diagonal: every lane enters on the top row or the left column (`lanes`), so between
  them they head every diagonal the page has and no two streaks
  can share a cell. Each drop crosses its own diagonal at one cell per row (`DRIFT`), painting a
  `\` shaft that trails `TRAIL` cells back up and to the left of its bright tip; period and phase
  come from a hash of the entry cell, so it needs no state: `view` paints whatever frame the
  `FrameIndexer` clock selects and `next_wake` reports when the loop should come back. The running
  version (`components::version_label`, shared with the session's version bar so the two cannot
  disagree) is stamped dim in the top-right corner: the right end of the top row, dropped whole when
  the viewport is narrower than the label, and painted *before* the wordmark so that on a terminal
  short enough to bring the two onto one row `BigText`'s opaque box is what wins. The *page* sweeps
  in rather than appearing whole: for its first `ENTRANCE` (900ms) it is wiped down the screen by a
  `tachyonfx` `fx::sweep_in`, its dark plate with it. The page is built in a `Buffer` of its own and
  only the cells the edge has reached are copied out, so what stands ahead of the edge is whatever the
  caller painted there — the empty frame for onboarding, the session for the screensaver, which is
  what makes the wipe a transition rather than a cut. The edge is read back off the built page by the
  colour `sweep_in` paints the cells it has not reached (the plate's own, on both channels), which is
  why nothing in the page is drawn in that colour (`the_plate_is_never_painted_as_ink`). The sweep's
  `randomness` — the tear that makes it read as irregular rather than as a straight wipe — is taken
  from the page at `ENTRANCE_TEAR_PERCENT` of its height, so a tall terminal is not wiped cleanly while
  a short one is shredded, and its gradient is deliberately narrow so the tear is what shapes the
  boundary. It is **rebuilt on every paint** and advanced straight to the age rather than kept alive
  between frames, which is what keeps the scene a pure function of its clocks
  (`the_entrance_is_a_pure_function_of_its_clock`); that works because `sweep_in` draws its jitter from a
  fixed seed. Past `ENTRANCE` the page is painted directly, so the steady state is the drawn page
  exactly rather than the effect's last frame of it. The plate rewrites the cells it covers, symbol as
  well as style: restyling alone would leave the glyphs of the screen underneath standing in the
  page's own ink, which is exactly the screen the screensaver is sweeping away. The two clocks — the
  rain's frame and the age the page reports — both arrive as arguments: `art` reads no clock of its
  own beyond the rain's, and the page that owns the lifespan keeps it.
- `src/tui/app.rs`: `welcome.open` is the single "onboarding is running" flag. `App::view` skips
  every session widget while it is set, `welcome_overlay(stage)` maps the stage onto the overlay
  that routes that page's keys, pastes, and registry events (`Welcome` for the interstitial pages,
  `AddProvider`/`ModelPicker` for the dialogs), and `set_welcome_stage` moves both together, by
  calling `Welcome::show` so the title's entrance clock moves with the page. The flow's transitions:
  form `Submit` sends `AddProvider`/`SetActiveProvider`/`ListModels` and stays
  on the form; `ModelsLoaded` pages on to the picker (or straight to `AllSet` when the provider
  lists nothing) and `ModelsError` returns to the form with the error; the picker's `Selected` and
  Esc-with-a-default land on `AllSet`; `AllSet`'s next key press finishes onboarding. Esc on the
  form's first page walks back to the title. Cancelling the quit prompt calls `close_overlay`,
  which re-points the overlay at the stage still showing, so it lands back on the page it was
  asked from.
- `src/tui/welcome.rs`: the two interstitial pages. `WelcomeStage` selects the page and `Welcome::show`
  is how it changes — it restarts the title's entrance clock, so stepping back to the title from the
  form sweeps it in again rather than showing a scene already drawn. That clock is the one piece of
  state the welcome keeps for the drawing: `art` reads none of its own, so the age is handed over on
  every paint and on every `animation_wake`.
- `src/tui/add_provider.rs` / `src/tui/model_picker.rs`: `dialog_rect(area, fullscreen, …)` picks
  between the centred popup and `fullscreen_rect`, the viewport inset by a small margin; the
  render loop's `set_viewport` geometry mirrors the same choice through
  `add_provider_list_height`/`model_picker_list_height`.
- `src/tui.rs`: the render loop wakes at the earliest of the spinners' next frame and the fullscreen
  scenes' (`App::animation_wake`, over the welcome art and the `/screensaver` scene), both wall-clock
  driven, so the title animates without any render-loop state. The scene's wake is the sooner of its
  rain and its entrance (`art::next_wake`), and the entrance's frames are the faster of the two — a
  gradient crossing the page in the eleven steps the rain's twelve frames a second would give it reads
  as dropped frames rather than as animation.

## `/screensaver`: the same scene over a live session

`/screensaver` plays the title page's artwork fullscreen over a live session, for as long as someone
wants to watch the rain. It shares the drawing and none of the flow.

- `src/tui/screensaver.rs`: `Screensaver` is `open` plus the scene, and the clock the scene's entrance
  sweep is drawn from (`opened_at`, recorded by `open`), so raising it again sweeps it in again.
  `map_event` maps any key press to `Close` except Ctrl+C, which asks to quit
  (`ScreensaverEffect::RequestQuit`): the scene has no input for Ctrl+C to clear, and a user reaching
  for it on an ambient screen wants out of the app rather than back in the session. `update` closes
  the model on `Close` and leaves it up on `RequestQuit`, so the prompt is a dialog over the scene.
- It is its own overlay (`Overlay::Screensaver`), not a mode of `Welcome`, because `welcome.open`
  means "onboarding is running": `reload_config` pages a title-page welcome on to `AllSet` when a
  config becomes connectable, and `App::view` skips the session for it. A screensaver sharing that
  flag would inherit onboarding's auto-advance from inside a live session.
- `App::view` paints the scene *instead of* the session — the page owns the screen, so a chat nobody
  is reading does not repaint on the rain's clock — then the shared `view_popups` tail (the quit
  prompt and the two transient popups), which onboarding uses too. The exception is the scene's own
  entrance: `Screensaver::sweeping_in` reports it, and while it holds `App::view` paints the session
  page under the scene, because that screen is what the wipe is carrying away. Once the page has
  landed there is nothing left to cover and the session stops repainting. Both this and the normal
  path paint through `view_session_page`, so the page being wiped is the page that was there.
  `close_overlay` re-points the overlay at the scene while it is open, so cancelling the quit prompt
  asked from it lands back on the scene rather than in the session.
- `App::run_command` refuses to raise it while onboarding is open: onboarding paints nothing beside
  its own page, so the scene would never reach the terminal. The command is unreachable there anyway
  (the composer is not live until onboarding finishes), and the guard only keeps two fullscreen
  flows from overlapping by accident.
- The caption belongs to the page, not the scene: `art::view` takes it as a parameter, so the title
  page can promise the provider form and the screensaver can promise the session back. `welcome.rs`
  owns `CONTINUE_CAPTION`, shared by both of its interstitial pages.
