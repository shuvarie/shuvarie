//! The `/screensaver` overlay: the onboarding title's scene, full screen over a
//! live session until a key press hands the screen back.
//!
//! It shares the drawing with onboarding's title page (`welcome::art`) but none
//! of its flow. There is no provider form behind it and no page to move on to,
//! so a key press is the whole interaction — where onboarding's title advances,
//! the scene here is dismissed. Ctrl+C keeps its app-wide meaning and asks to
//! quit, as it does on every page of onboarding.
//!
//! Like onboarding it paints an opaque fullscreen page: `App::view` skips the
//! session rather than painting it under the rain, so a long chat does not
//! repaint sixty times a minute behind a scene nobody is reading. The one
//! exception is the scene's own entrance, which arrives *over* the session — see
//! [`Screensaver::sweeping_in`] — because the screen it wipes away is the session,
//! and it has to be painted for there to be anything to wipe. It also outlives a
//! dialog opened over it, so cancelling the quit prompt lands back on the scene
//! instead of dropping the user into the session.
//!
//! The scene sweeps in as it comes up, as it does on the title page. That needs a
//! clock, and the page is where it belongs: the scene reads none of its own, so
//! the screensaver records when it was raised and reports the age on every paint.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

use super::welcome::art;

/// What a key press does here — the one thing this page can promise. It is not
/// onboarding's caption: that one pages on to the provider form, and this one
/// cannot.
const CAPTION: &str = "Press any key to exit";

/// How many entrance lengths [`Screensaver::hold_at_start`] parks the sweep in
/// the future: a test-only margin, wide enough that no plausible scheduling gap
/// can settle the sweep before the test draws.
#[cfg(test)]
const HELD_ENTRANCES: u32 = 100;

pub enum ScreensaverMessage {
    /// A key press dismissed the scene.
    Close,
    /// Ctrl+C: the app's quit prompt, as on every other screen.
    RequestQuit,
}

pub enum ScreensaverEffect {
    /// Open the quit prompt over the scene.
    RequestQuit,
}

pub struct Screensaver {
    open: bool,
    /// When the scene last came up. The entrance sweep is drawn from it, so
    /// raising the scene again sweeps it in again. `None` while it is down.
    opened_at: Option<Instant>,
}

impl Screensaver {
    pub fn new() -> Self {
        Self {
            open: false,
            opened_at: None,
        }
    }

    /// Raises the scene over the session, sweeping in from the top of the page.
    pub fn open(&mut self) {
        self.open = true;
        self.opened_at = Some(Instant::now());
    }

    /// Drops the scene: the session takes the screen back.
    pub fn close(&mut self) {
        self.open = false;
        self.opened_at = None;
    }

    /// Reads the settled page rather than the sweep, which a test cannot wait out:
    /// the entrance runs off the wall clock, and back-dating it is the only way to
    /// draw past its end.
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        if self.open {
            self.opened_at = Some(Instant::now() - art::ENTRANCE);
        }
    }

    /// Holds the scene at the very start of its entrance, so a test can read the
    /// screen it has not covered yet. A loaded machine can otherwise be hundreds
    /// of milliseconds into the sweep by the time a test draws it; an instant in
    /// the future reads as an age of zero for as long as it stays there.
    ///
    /// The margin is far larger than one entrance: the whole suite runs its
    /// tests in parallel, and a thread that spawns a child process (or waits on
    /// a 3-second timeout) can be descheduled for longer than [`art::ENTRANCE`],
    /// which would settle the sweep and wipe the session before the draw.
    #[cfg(test)]
    pub(crate) fn hold_at_start(&mut self) {
        if self.open {
            self.opened_at = Some(Instant::now() + art::ENTRANCE * HELD_ENTRANCES);
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the scene is still sweeping in, and so still covering the screen it
    /// was raised over rather than standing on it: `App::view` paints the session
    /// underneath while this holds, and stops once the page has landed.
    pub fn sweeping_in(&self) -> bool {
        self.open && self.age() < art::ENTRANCE
    }

    /// How long the scene has been up — the clock its entrance sweep runs on.
    /// Zero while it is down, which draws the settled scene.
    fn age(&self) -> Duration {
        self.opened_at.map_or(Duration::ZERO, |at| at.elapsed())
    }

    /// The wall-clock delay until the scene's next frame — the render loop's
    /// wake. `None` while it is down: the rain is all that moves here.
    pub fn animation_wake(&self) -> Option<Duration> {
        self.open.then(|| art::next_wake(self.age())).flatten()
    }

    /// Any key press dismisses the scene. Ctrl+C is the exception: it asks to
    /// quit instead, because the scene has no input for Ctrl+C to clear and a
    /// user reaching for it on an ambient screen wants out of the app rather
    /// than back in the session. A key release is not a press.
    pub fn map_event(&self, key: &KeyEvent) -> Option<ScreensaverMessage> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if key.modifiers.contains(Modifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(ScreensaverMessage::RequestQuit);
        }
        Some(ScreensaverMessage::Close)
    }

    pub fn update(&mut self, msg: ScreensaverMessage) -> Option<ScreensaverEffect> {
        match msg {
            ScreensaverMessage::Close => {
                self.close();
                None
            }
            // Asking to quit leaves the scene up: the prompt is a dialog over
            // it, and cancelling comes back here.
            ScreensaverMessage::RequestQuit => Some(ScreensaverEffect::RequestQuit),
        }
    }

    /// Paints the scene fullscreen. A closed screensaver draws nothing at all,
    /// so a stray call cannot cover the session.
    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.open {
            art::view(frame, area, self.age(), CAPTION);
        }
    }
}

impl Default for Screensaver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::buffer::Buffer;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::CONTROL)
    }

    /// The scene paints over a marked screen: what `App::view` puts under it before
    /// the sweep runs, so a cell still carrying the marker is one the page has not
    /// covered.
    const UNDERLAY: &str = "~";

    /// A scene that has long since swept in. The tests that read the finished
    /// page are not measuring the entrance, so they draw past its end.
    fn raised() -> Screensaver {
        let mut screensaver = Screensaver::new();
        screensaver.open();
        screensaver.settle();
        screensaver
    }

    #[test]
    fn any_key_press_but_ctrl_c_dismisses_the_scene() {
        let screensaver = raised();
        for code in [
            KeyCode::Escape,
            KeyCode::Enter,
            KeyCode::Char('q'),
            KeyCode::Char(' '),
            KeyCode::Tab,
        ] {
            assert!(
                matches!(
                    screensaver.map_event(&key(code)),
                    Some(ScreensaverMessage::Close)
                ),
                "{code:?} dismisses the scene"
            );
        }

        let mut release = key(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        assert!(
            screensaver.map_event(&release).is_none(),
            "a key release is not a press"
        );

        assert!(
            matches!(
                screensaver.map_event(&ctrl(KeyCode::Char('c'))),
                Some(ScreensaverMessage::RequestQuit)
            ),
            "Ctrl+C asks to quit rather than dismissing"
        );
        assert!(screensaver.is_open(), "mapping a key changes nothing");
    }

    #[test]
    fn a_dismissal_closes_the_scene_and_a_quit_request_does_not() {
        let mut screensaver = raised();

        assert!(
            screensaver
                .update(ScreensaverMessage::RequestQuit)
                .is_some()
        );
        assert!(
            screensaver.is_open(),
            "the quit prompt is a dialog over the scene"
        );

        assert!(screensaver.update(ScreensaverMessage::Close).is_none());
        assert!(!screensaver.is_open(), "a key press hands the screen back");
    }

    #[test]
    fn the_rain_only_runs_while_the_scene_is_up() {
        let mut screensaver = Screensaver::new();
        assert!(
            screensaver.animation_wake().is_none(),
            "a closed scene never wakes the loop"
        );
        screensaver.open();
        assert!(
            screensaver.animation_wake().is_some(),
            "the open scene drives the rain"
        );
        screensaver.close();
        assert!(screensaver.animation_wake().is_none());
    }

    fn draw(screensaver: &Screensaver, width: u16, height: u16) -> Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                for pos in frame.area().positions() {
                    frame.buffer_mut()[pos].set_symbol(UNDERLAY);
                }
                screensaver.view(frame, frame.area());
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Whether the entrance has reached `(x, y)`. Ahead of its edge the page has
    /// not painted at all, so the screen underneath still stands there.
    fn reached(buf: &Buffer, x: u16, y: u16) -> bool {
        buf[(x, y)].symbol() != UNDERLAY
    }

    /// How much of the page the entrance has reached.
    fn arrived(buf: &Buffer) -> usize {
        (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| reached(buf, x, y))
            .count()
    }

    /// The scene sweeps in as it comes up, and comes up sweeping every time: the
    /// entrance is drawn from when the scene was raised, and the screensaver is
    /// what records that.
    #[test]
    fn raising_the_scene_sweeps_it_in_again() {
        let mut screensaver = Screensaver::new();
        screensaver.open();
        assert!(
            screensaver.sweeping_in(),
            "a scene that has just come up is still sweeping in"
        );
        screensaver.hold_at_start();
        assert_eq!(
            arrived(&draw(&screensaver, 80, 24)),
            0,
            "and has not covered the screen it was raised over"
        );
        assert_eq!(
            arrived(&draw(&raised(), 80, 24)),
            80 * 24,
            "and then the whole page arrives"
        );

        // Dropping it and raising it again starts the entrance over rather than
        // showing a scene already drawn.
        screensaver.settle();
        assert!(!screensaver.sweeping_in(), "the page has landed");
        screensaver.close();
        screensaver.open();
        assert!(
            screensaver.sweeping_in(),
            "raising the scene again sweeps it in again"
        );
    }

    fn text(buf: &Buffer, width: u16) -> String {
        (0..buf.area.height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_scene_paints_the_wordmark_under_its_own_caption() {
        let buf = draw(&raised(), 80, 24);
        let screen = text(&buf, 80);
        assert!(screen.contains(CAPTION), "{screen}");
        assert!(
            screen.lines().any(|line| line.contains('█')),
            "the wordmark paints: {screen}"
        );
        assert_eq!(
            buf[(0, 0)].style().bg,
            Some(crate::tui::theme::bg()),
            "the page fills with the theme background"
        );
    }

    #[test]
    fn a_closed_screensaver_paints_nothing() {
        let buf = draw(&Screensaver::new(), 80, 24);
        assert!(
            buf.area.positions().all(|pos| !reached(&buf, pos.x, pos.y)),
            "an unopened scene never covers the session"
        );
    }

    /// `sweeping_in` is what `App::view` reads to decide whether the session is
    /// painted under the scene: it reports the entrance, and nothing else. The
    /// screen it leaves uncovered is the art's, which `welcome::art` tests.
    #[test]
    fn the_scene_reports_while_it_is_still_sweeping_in() {
        let mut screensaver = Screensaver::new();
        assert!(
            !screensaver.sweeping_in(),
            "a scene that is down is not covering anything"
        );

        screensaver.open();
        assert!(
            screensaver.sweeping_in(),
            "a scene just raised is still coming up"
        );

        screensaver.settle();
        assert!(
            !screensaver.sweeping_in(),
            "a scene that has arrived stands on its own page"
        );
    }
}
