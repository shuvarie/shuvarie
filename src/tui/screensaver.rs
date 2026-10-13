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
//! repaint sixty times a minute behind a scene nobody is reading. It also
//! outlives a dialog opened over it, so cancelling the quit prompt lands back on
//! the scene instead of dropping the user into the session.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

use super::welcome::art;

/// What a key press does here — the one thing this page can promise. It is not
/// onboarding's caption: that one pages on to the provider form, and this one
/// cannot.
const CAPTION: &str = "Press any key to exit";

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
}

impl Screensaver {
    pub fn new() -> Self {
        Self { open: false }
    }

    /// Raises the scene over the session.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Drops the scene: the session takes the screen back.
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The wall-clock delay until the scene's next frame — the render loop's
    /// wake. `None` while it is down: the rain is all that moves here.
    pub fn animation_wake(&self) -> Option<Duration> {
        self.open.then(art::next_wake).flatten()
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
            art::view(frame, area, CAPTION);
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

    fn raised() -> Screensaver {
        let mut screensaver = Screensaver::new();
        screensaver.open();
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
            .draw(|frame| screensaver.view(frame, frame.area()))
            .unwrap();
        terminal.backend().buffer().clone()
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
            text(&buf, 80).trim().is_empty(),
            "an unopened scene never covers the session"
        );
    }
}
