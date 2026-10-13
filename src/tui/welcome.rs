//! The onboarding flow that replaces the session until a provider is ready.
//! It pages fullscreen through an animated title, the first-provider form, the
//! model picker, and a closing "all set" page — nothing below it is painted or
//! interactive, so there is no way into a session that has no provider behind
//! it. Ctrl+C asks to quit from every page, and Esc from the two interstitial
//! ones — the form and the picker have a `Cancel`/accept meaning for it.
//!
//! The welcome owns only the two interstitial pages ([`art`] does the drawing);
//! the form and the picker paint themselves fullscreen, and `App` forwards
//! their keys while the stage selects them.

use std::time::Duration;

use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

use super::theme;

mod art;

/// Which fullscreen page onboarding is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WelcomeStage {
    /// The animated title: the scene, the `Shuvarie` wordmark, and the
    /// continue caption.
    Intro,
    /// The first-provider form.
    AddProvider,
    /// The model picker for the provider just added.
    ModelPicker,
    /// The closing page: a model is configured, and any key hands the terminal
    /// over to the session.
    AllSet,
}

pub enum WelcomeMessage {
    /// A key press on one of the interstitial pages advances the flow.
    Advance,
    RequestQuit,
}

pub enum WelcomeEffect {
    /// The title page was dismissed: open the first-provider form.
    AddProvider,
    /// Onboarding is done: the session takes the screen.
    Finish,
    RequestQuit,
}

pub struct Welcome {
    pub open: bool,
    pub stage: WelcomeStage,
}

impl Welcome {
    pub fn new() -> Self {
        Self {
            open: false,
            stage: WelcomeStage::Intro,
        }
    }

    /// Raises onboarding, always from its first page.
    pub fn open(&mut self) {
        self.open = true;
        self.stage = WelcomeStage::Intro;
    }

    /// Drops onboarding: the session takes the screen.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// The wall-clock delay until the title art's next frame. `None` when
    /// onboarding is closed or showing a page that does not animate.
    pub fn animation_wake(&self) -> Option<Duration> {
        (self.open && self.stage == WelcomeStage::Intro)
            .then(art::next_wake)
            .flatten()
    }

    /// The interstitial pages' key handling: Ctrl+C and Esc ask to quit, and
    /// any other key press moves the flow on. The form and the picker own their
    /// keys, so nothing else is mapped on their pages — the router forwards to
    /// them instead.
    pub fn map_event(&self, key: &KeyEvent) -> Option<WelcomeMessage> {
        if key.modifiers.contains(Modifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(WelcomeMessage::RequestQuit);
        }
        match self.stage {
            WelcomeStage::Intro | WelcomeStage::AllSet => {
                // Esc asks to quit here instead of paging on: it is the key a
                // user looking for a way out reaches for first, and there is
                // nothing else on these pages for it to mean. On the form and
                // the picker it stays `Cancel`/accept and is left alone.
                if key.code == KeyCode::Escape && key.kind == KeyEventKind::Press {
                    return Some(WelcomeMessage::RequestQuit);
                }
                (key.kind == KeyEventKind::Press).then_some(WelcomeMessage::Advance)
            }
            WelcomeStage::AddProvider | WelcomeStage::ModelPicker => None,
        }
    }

    pub fn update(&mut self, msg: WelcomeMessage) -> Option<WelcomeEffect> {
        match msg {
            WelcomeMessage::RequestQuit => Some(WelcomeEffect::RequestQuit),
            WelcomeMessage::Advance => match self.stage {
                WelcomeStage::Intro => {
                    self.stage = WelcomeStage::AddProvider;
                    Some(WelcomeEffect::AddProvider)
                }
                WelcomeStage::AllSet => {
                    self.close();
                    Some(WelcomeEffect::Finish)
                }
                WelcomeStage::AddProvider | WelcomeStage::ModelPicker => None,
            },
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        match self.stage {
            WelcomeStage::Intro => art::view(frame, area),
            WelcomeStage::AllSet => self.view_all_set(frame, area),
            // The form and the picker paint themselves fullscreen; the welcome
            // only owns the pages between them.
            WelcomeStage::AddProvider | WelcomeStage::ModelPicker => {}
        }
    }

    /// The closing page: onboarding is done, one key press from the session.
    fn view_all_set(&self, frame: &mut Frame<'_>, area: Rect) {
        let buf = frame.buffer_mut();
        buf.set_style(area, Style::new().bg(theme::bg()).fg(theme::text()));

        let lines = [
            Line::from(Span::styled("✓", Style::new().fg(theme::success()))),
            Line::from(""),
            Line::from(Span::styled(
                "You're all set!",
                Style::new()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Press any key to continue",
                Style::new().fg(theme::text_muted()),
            )),
        ];
        let height = lines.len() as u16;
        let block = Rect::new(
            area.x,
            area.y + area.height.saturating_sub(height) / 2,
            area.width,
            height,
        );
        Paragraph::new(Vec::from(lines))
            .alignment(Alignment::Center)
            .render(block, buf);
    }
}

impl Default for Welcome {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::CONTROL)
    }

    #[test]
    fn any_key_press_but_escape_advances_the_interstitial_pages() {
        let mut welcome = Welcome::new();
        welcome.open();

        assert!(matches!(
            welcome.map_event(&key(KeyCode::Char('q'))),
            Some(WelcomeMessage::Advance)
        ));
        assert!(matches!(
            welcome.map_event(&key(KeyCode::Enter)),
            Some(WelcomeMessage::Advance)
        ));
        let mut release = key(KeyCode::Char('q'));
        release.kind = KeyEventKind::Release;
        assert!(
            welcome.map_event(&release).is_none(),
            "a key release is not a press"
        );

        // Intro pages on to the form…
        assert!(matches!(
            welcome.update(WelcomeMessage::Advance),
            Some(WelcomeEffect::AddProvider)
        ));
        assert_eq!(welcome.stage, WelcomeStage::AddProvider);

        // …and the closing page hands over to the session.
        welcome.stage = WelcomeStage::AllSet;
        assert!(matches!(
            welcome.update(WelcomeMessage::Advance),
            Some(WelcomeEffect::Finish)
        ));
        assert!(!welcome.open, "finishing closes onboarding");
    }

    #[test]
    fn ctrl_c_asks_to_quit_from_every_page() {
        for stage in [
            WelcomeStage::Intro,
            WelcomeStage::AddProvider,
            WelcomeStage::ModelPicker,
            WelcomeStage::AllSet,
        ] {
            let mut welcome = Welcome::new();
            welcome.open();
            welcome.stage = stage;
            assert!(
                matches!(
                    welcome.map_event(&ctrl(KeyCode::Char('c'))),
                    Some(WelcomeMessage::RequestQuit)
                ),
                "Ctrl+C on {stage:?}"
            );
            assert!(matches!(
                welcome.update(WelcomeMessage::RequestQuit),
                Some(WelcomeEffect::RequestQuit)
            ));
            assert!(welcome.open, "asking to quit leaves onboarding open");
        }
    }

    #[test]
    fn escape_asks_to_quit_on_the_interstitial_pages_only() {
        for stage in [WelcomeStage::Intro, WelcomeStage::AllSet] {
            let mut welcome = Welcome::new();
            welcome.open();
            welcome.stage = stage;
            assert!(
                matches!(
                    welcome.map_event(&key(KeyCode::Escape)),
                    Some(WelcomeMessage::RequestQuit)
                ),
                "Esc on {stage:?}"
            );
            assert!(matches!(
                welcome.update(WelcomeMessage::RequestQuit),
                Some(WelcomeEffect::RequestQuit)
            ));
            assert_eq!(welcome.stage, stage, "asking to quit does not page on");
        }

        // The form maps Esc to `Cancel` and the picker to a close: onboarding
        // must not swallow either.
        for stage in [WelcomeStage::AddProvider, WelcomeStage::ModelPicker] {
            let mut welcome = Welcome::new();
            welcome.open();
            welcome.stage = stage;
            assert!(
                welcome.map_event(&key(KeyCode::Escape)).is_none(),
                "Esc is the sub-page's own key on {stage:?}"
            );
        }
    }

    #[test]
    fn the_form_and_picker_pages_keep_their_own_keys() {
        for stage in [WelcomeStage::AddProvider, WelcomeStage::ModelPicker] {
            let mut welcome = Welcome::new();
            welcome.open();
            welcome.stage = stage;
            assert!(welcome.map_event(&key(KeyCode::Enter)).is_none());
            assert!(welcome.map_event(&key(KeyCode::Char('a'))).is_none());
            // The sub-page cannot advance the flow on its own.
            assert!(welcome.update(WelcomeMessage::Advance).is_none());
            assert_eq!(welcome.stage, stage);
        }
    }

    #[test]
    fn opening_resets_to_the_title_page() {
        let mut welcome = Welcome::new();
        welcome.open();
        welcome.stage = WelcomeStage::AllSet;
        welcome.close();
        assert!(!welcome.open);
        assert!(welcome.animation_wake().is_none(), "closed: no animation");

        welcome.open();
        assert_eq!(welcome.stage, WelcomeStage::Intro);
        assert!(
            welcome.animation_wake().is_some(),
            "the title page drives the rain"
        );
    }

    #[test]
    fn only_the_title_page_animates() {
        let mut welcome = Welcome::new();
        welcome.open();
        assert!(welcome.animation_wake().is_some());
        for stage in [
            WelcomeStage::AddProvider,
            WelcomeStage::ModelPicker,
            WelcomeStage::AllSet,
        ] {
            welcome.stage = stage;
            assert!(welcome.animation_wake().is_none(), "{stage:?} is static");
        }
    }

    fn draw(stage: WelcomeStage, width: u16, height: u16) -> Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        let mut welcome = Welcome::new();
        welcome.open();
        welcome.stage = stage;
        terminal
            .draw(|frame| welcome.view(frame, frame.area()))
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
    fn the_title_page_paints_the_wordmark_and_the_continue_caption() {
        let buf = draw(WelcomeStage::Intro, 80, 24);
        let screen = text(&buf, 80);
        assert!(screen.contains("Press any key to continue"), "{screen}");
        assert!(
            screen.lines().any(|line| line.contains('█')),
            "the wordmark paints: {screen}"
        );
        assert_eq!(
            buf[(0, 0)].style().bg,
            Some(theme::bg()),
            "the page fills with the theme background"
        );
    }

    #[test]
    fn the_all_set_page_is_static_and_says_so() {
        let buf = draw(WelcomeStage::AllSet, 80, 24);
        let screen = text(&buf, 80);
        assert!(screen.contains("You're all set!"), "{screen}");
        assert!(screen.contains("Press any key to continue"), "{screen}");
        assert!(
            !screen.contains('█'),
            "the closing page carries no wordmark: {screen}"
        );
    }

    #[test]
    fn the_form_and_picker_pages_paint_nothing_of_their_own() {
        for stage in [WelcomeStage::AddProvider, WelcomeStage::ModelPicker] {
            let buf = draw(stage, 80, 24);
            assert!(
                !text(&buf, 80).contains("You're all set!"),
                "{stage:?} paints no welcome chrome"
            );
        }
    }
}
