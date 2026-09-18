use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use termina::event::{KeyCode, KeyEvent, KeyEventKind};

use super::add_provider::centered_rect;
use super::theme;

/// Device-flow sign-in messages for the TUI auth popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMessage {
    /// A device-flow sign-in prompt arrived: show the verification URL and
    /// user code so the user can authorize in a browser.
    Prompt {
        provider: String,
        verification_uri: String,
        user_code: String,
    },
    /// An OAuth sign-in completed successfully for `provider`.
    Succeeded { provider: String },
    /// An OAuth sign-in failed for `provider` with `error`.
    Failed { provider: String, error: String },
    /// Popup keybind: dismiss the prompt (sign-in keeps polling in the
    /// background and the outcome still arrives afterwards).
    Dismiss,
    /// Popup keybind: launch the verification URL in the platform browser.
    OpenBrowser { url: String },
}

/// Transient OAuth device-flow prompt (ChatGPT/Codex, GitHub Copilot). Not
/// part of the overlay stack: it paints over every overlay while the provider
/// polls for the browser authorization. Any key dismisses it — sign-in keeps
/// running and the outcome (`AuthSuccess`/`AuthFailed`) still arrives as a
/// notice — except `o`/Enter, which launches the verification URL in a
/// browser.
pub struct AuthPopup {
    pub open: bool,
    provider: String,
    verification_uri: String,
    user_code: String,
}

impl AuthPopup {
    pub fn new() -> Self {
        Self {
            open: false,
            provider: String::new(),
            verification_uri: String::new(),
            user_code: String::new(),
        }
    }

    pub fn start(&mut self, provider: String, verification_uri: String, user_code: String) {
        self.provider = provider;
        self.verification_uri = verification_uri;
        self.user_code = user_code;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Whether this popup is showing a prompt for `provider`.
    pub fn matches(&self, provider: &str) -> bool {
        self.open && self.provider == provider
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AuthMessage> {
        if key.kind != KeyEventKind::Press || key.code == KeyCode::CapsLock {
            return None;
        }
        match key.code {
            KeyCode::Char('o') | KeyCode::Enter => Some(AuthMessage::OpenBrowser {
                url: self.verification_uri.clone(),
            }),
            _ => Some(AuthMessage::Dismiss),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        let title = format!("Sign in with {}", self.provider);
        let block = theme::overlay_block(&title);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let body = vec![
            Line::from("Visit this URL in a browser:"),
            Line::from(""),
            Line::from(Span::styled(
                self.verification_uri.clone(),
                Style::new().fg(theme::accent()).bold(),
            )),
            Line::from(""),
            Line::from("Then enter the code:"),
            Line::from(""),
            Line::from(Span::styled(
                self.user_code.clone(),
                Style::new().fg(theme::accent()).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "The provider finishes sign-in automatically while it polls.",
                Style::new().fg(theme::text_muted()),
            )),
            Line::from(Span::styled(
                "Do not share this device code.",
                Style::new().fg(theme::text_muted()),
            )),
        ];
        frame.render_widget(
            Paragraph::new(body).wrap(Wrap { trim: false }),
            Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            ),
        );

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("o", "open in browser"),
                ("any key", "dismiss"),
            ]))
            .fg(theme::text_muted()),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

impl Default for AuthPopup {
    fn default() -> Self {
        Self::new()
    }
}

/// Launch the platform browser at `url`, detached from this process.
/// Best-effort: a headless environment has no default browser and the user
/// copies the URL manually instead.
pub fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        // `start`'s first quoted argument is the window title; passing an
        // empty one keeps a URL with special characters intact.
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    const URL: &str = "https://auth.openai.com/codex/device";

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    fn open_popup() -> AuthPopup {
        let mut popup = AuthPopup::new();
        popup.start("ChatGPT".into(), URL.into(), "ABCD-1234".into());
        popup
    }

    #[test]
    fn o_and_enter_open_the_browser_other_keys_dismiss() {
        let popup = open_popup();
        for code in [KeyCode::Char('o'), KeyCode::Enter] {
            match popup.map_event(&key(code)).expect("mapped") {
                AuthMessage::OpenBrowser { url } => assert_eq!(url, URL),
                other => panic!("unexpected message: {other:?}"),
            }
        }
        for code in [
            KeyCode::Escape,
            KeyCode::Char('x'),
            KeyCode::Backspace,
            KeyCode::Down,
        ] {
            assert!(
                matches!(popup.map_event(&key(code)), Some(AuthMessage::Dismiss)),
                "{code:?} should dismiss"
            );
        }
    }

    #[test]
    fn release_events_and_caps_lock_are_ignored() {
        let popup = open_popup();
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..key(KeyCode::Char('o'))
        };
        assert!(popup.map_event(&release).is_none());
        assert!(popup.map_event(&key(KeyCode::CapsLock)).is_none());
    }

    #[test]
    fn matches_tracks_provider_and_closed_state() {
        let mut popup = open_popup();
        assert!(popup.matches("ChatGPT"));
        assert!(!popup.matches("GitHub Copilot"));
        popup.close();
        assert!(!popup.matches("ChatGPT"), "closed popup matches nothing");
    }

    #[test]
    fn start_replaces_the_active_prompt() {
        let mut popup = open_popup();
        popup.start(
            "GitHub Copilot".into(),
            "https://github.com/login/device".into(),
            "XXXX-YYYY".into(),
        );
        assert!(popup.matches("GitHub Copilot"));
        assert!(!popup.matches("ChatGPT"));
        match popup.map_event(&key(KeyCode::Char('o'))).expect("mapped") {
            AuthMessage::OpenBrowser { url } => {
                assert_eq!(url, "https://github.com/login/device");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
