//! The decision-provider dialog (`/decision-provider`).
//!
//! A decision model is not a generative LLM, so it does not go through the
//! add-provider wizard: there is no Selune catalog entry to pick, no transport
//! to choose, and no OAuth. One System One endpoint answers with one protocol,
//! so the whole connection is four fields — a name, an endpoint, an optional
//! credential, and a model.
//!
//! The model is free text. Decision models are not enumerable: Selune has no
//! registry for them and Ollama does not list them through its CLI or official
//! libraries, so nothing here can validate the name and the first call is what
//! reports a bad one.
//!
//! Submitting writes two halves of one selection — the connection and the
//! active `decision { provider; model }` — because a decision connection is
//! unusable without a model. Re-opening the dialog seeds it from the active
//! connection, so submitting an unchanged form edits that connection in place
//! rather than registering a second one.

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Paragraph, Wrap};
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::add_provider::centered_rect;
use super::components::InputBuffer;
use super::{popup, theme};

/// The form's fields, in Tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionFormField {
    Name,
    BaseUrl,
    ApiKey,
    Model,
}

#[derive(Debug, PartialEq)]
pub enum AddDecisionProviderMessage {
    Input(char),
    Paste(String),
    Backspace,
    Delete,
    KillToEnd,
    NextField,
    PrevField,
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    Submit,
    Cancel,
}

#[derive(Debug, PartialEq)]
pub enum AddDecisionProviderOutcome {
    None,
    Cancel,
    Submit {
        name: String,
        /// Required: a connection with no endpoint cannot resolve a request.
        base_url: String,
        /// Optional: a local System One server (Ollama) ignores authentication,
        /// and the client substitutes a placeholder token.
        api_key: Option<String>,
        /// Required, and free text — see the module docs.
        model: String,
    },
}

/// The dialog's fields and focus. `InputBuffer` is neither `Clone` nor
/// `Debug`, so this struct derives neither — like `AddProviderForm`.
pub struct AddDecisionProviderForm {
    name: InputBuffer,
    base_url: InputBuffer,
    api_key: InputBuffer,
    model: InputBuffer,
    field: DecisionFormField,
    error: Option<String>,
    /// The registered connection names, so the form can say whether submitting
    /// adds a connection or updates one.
    existing_names: Vec<String>,
}

impl AddDecisionProviderForm {
    /// A blank form over the given registered connection names.
    pub fn new(existing_names: &[String]) -> Self {
        Self {
            name: InputBuffer::new(),
            base_url: InputBuffer::new(),
            api_key: InputBuffer::new(),
            model: InputBuffer::new(),
            field: DecisionFormField::Name,
            error: None,
            existing_names: existing_names.to_vec(),
        }
    }

    /// Seed the fields from the active connection, so the dialog edits it in
    /// place instead of registering a duplicate under a second name.
    pub fn prefill(
        mut self,
        name: &str,
        base_url: Option<&str>,
        api_key: Option<&str>,
        model: &str,
    ) -> Self {
        self.name.set(name);
        self.base_url.set(base_url.unwrap_or_default());
        self.api_key.set(api_key.unwrap_or_default());
        self.model.set(model);
        self
    }

    /// Whether the typed name is already registered, i.e. submitting updates
    /// that connection rather than adding one.
    fn is_existing(&self) -> bool {
        let name = self.name.value.trim();
        !name.is_empty() && self.existing_names.iter().any(|known| known == name)
    }

    fn next_field(&mut self) {
        self.field = match self.field {
            DecisionFormField::Name => DecisionFormField::BaseUrl,
            DecisionFormField::BaseUrl => DecisionFormField::ApiKey,
            DecisionFormField::ApiKey => DecisionFormField::Model,
            DecisionFormField::Model => DecisionFormField::Name,
        };
    }

    fn prev_field(&mut self) {
        self.field = match self.field {
            DecisionFormField::Name => DecisionFormField::Model,
            DecisionFormField::BaseUrl => DecisionFormField::Name,
            DecisionFormField::ApiKey => DecisionFormField::BaseUrl,
            DecisionFormField::Model => DecisionFormField::ApiKey,
        };
    }

    fn field_buf(&mut self, field: DecisionFormField) -> &mut InputBuffer {
        match field {
            DecisionFormField::Name => &mut self.name,
            DecisionFormField::BaseUrl => &mut self.base_url,
            DecisionFormField::ApiKey => &mut self.api_key,
            DecisionFormField::Model => &mut self.model,
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AddDecisionProviderMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('b') => Some(AddDecisionProviderMessage::Left),
                KeyCode::Char('f') => Some(AddDecisionProviderMessage::Right),
                KeyCode::Char('a') => Some(AddDecisionProviderMessage::Home),
                KeyCode::Char('e') => Some(AddDecisionProviderMessage::End),
                KeyCode::Char('d') => Some(AddDecisionProviderMessage::Delete),
                KeyCode::Char('h') => Some(AddDecisionProviderMessage::Backspace),
                KeyCode::Char('k') => Some(AddDecisionProviderMessage::KillToEnd),
                _ => None,
            };
        }
        if alt(key) {
            return match key.code {
                KeyCode::Char('b') => Some(AddDecisionProviderMessage::LeftWord),
                KeyCode::Char('f') => Some(AddDecisionProviderMessage::RightWord),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(AddDecisionProviderMessage::Cancel),
            KeyCode::Tab => Some(AddDecisionProviderMessage::NextField),
            KeyCode::BackTab => Some(AddDecisionProviderMessage::PrevField),
            KeyCode::Enter => Some(AddDecisionProviderMessage::Submit),
            KeyCode::Backspace => Some(AddDecisionProviderMessage::Backspace),
            KeyCode::Left => Some(AddDecisionProviderMessage::Left),
            KeyCode::Right => Some(AddDecisionProviderMessage::Right),
            KeyCode::Home => Some(AddDecisionProviderMessage::Home),
            KeyCode::End => Some(AddDecisionProviderMessage::End),
            KeyCode::Char(c) => Some(AddDecisionProviderMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: AddDecisionProviderMessage) -> AddDecisionProviderOutcome {
        match msg {
            AddDecisionProviderMessage::Cancel => AddDecisionProviderOutcome::Cancel,
            AddDecisionProviderMessage::NextField => {
                self.next_field();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::PrevField => {
                self.prev_field();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Input(c) => {
                self.field_buf(self.field).push(c);
                self.error = None;
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Paste(text) => {
                let flat = super::components::flatten_newlines(&text);
                self.field_buf(self.field).insert_str(&flat);
                self.error = None;
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Backspace => {
                self.field_buf(self.field).backspace();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Delete => {
                self.field_buf(self.field).delete();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::KillToEnd => {
                self.field_buf(self.field).kill_to_end();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Left => {
                self.field_buf(self.field).left();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Right => {
                self.field_buf(self.field).right();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::LeftWord => {
                self.field_buf(self.field).left_word();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::RightWord => {
                self.field_buf(self.field).right_word();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Home => {
                self.field_buf(self.field).home();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::End => {
                self.field_buf(self.field).end();
                AddDecisionProviderOutcome::None
            }
            AddDecisionProviderMessage::Submit => self.submit(),
        }
    }

    /// Validate the fields and hand back the connection. The checks mirror what
    /// the decision client requires, so a fixable mistake is reported here
    /// rather than as a request failure on the first command.
    fn submit(&mut self) -> AddDecisionProviderOutcome {
        let name = self.name.value.trim().to_string();
        if name.is_empty() {
            self.error = Some("Name is required".into());
            return AddDecisionProviderOutcome::None;
        }
        let base_url = self.base_url.value.trim().to_string();
        if base_url.is_empty() {
            self.error = Some("Endpoint URL is required".into());
            return AddDecisionProviderOutcome::None;
        }
        if !has_scheme(&base_url) {
            // A scheme-less entry would reach the HTTP client as a malformed
            // URL, which reports far less than this does.
            self.error = Some("Endpoint URL must start with http:// or https://".into());
            return AddDecisionProviderOutcome::None;
        }
        if !has_host(&base_url) {
            // `http://` alone passes the scheme check but names nothing.
            self.error = Some("Endpoint URL must name a host".into());
            return AddDecisionProviderOutcome::None;
        }
        let model = self.model.value.trim().to_string();
        if model.is_empty() {
            self.error = Some("Model is required".into());
            return AddDecisionProviderOutcome::None;
        }
        let api_key = self.api_key.value.trim().to_string();
        AddDecisionProviderOutcome::Submit {
            name,
            base_url,
            api_key: (!api_key.is_empty()).then_some(api_key),
            model,
        }
    }

    fn field_line(
        &self,
        label: &str,
        buffer: &InputBuffer,
        field: DecisionFormField,
        hint: &str,
    ) -> Line<'static> {
        let label_style = Style::new().fg(theme::text_dim());
        let hint_style = Style::new().fg(theme::text_muted());
        let active_style = Style::new().fg(theme::accent());
        let inactive_style = Style::new().fg(theme::text_dim());
        let cursor_style = Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::REVERSED);

        let is_active = self.field == field;

        let mut spans = vec![Span::styled(format!("{label:<12}"), label_style)];

        if is_active {
            let chars: Vec<char> = buffer.value.chars().collect();
            let cursor_idx = buffer.cursor_char_index();
            for (i, c) in chars.iter().enumerate() {
                let style = if i == cursor_idx {
                    cursor_style
                } else {
                    active_style
                };
                spans.push(Span::styled(c.to_string(), style));
            }
            if cursor_idx >= chars.len() {
                spans.push(Span::styled(" ".to_string(), cursor_style));
            }
        } else if buffer.value.is_empty() {
            // An unfocused, empty field still needs a cell so the label column
            // and the hint below stay aligned with the filled rows.
            spans.push(Span::styled(" ".to_string(), inactive_style));
        } else {
            spans.push(Span::styled(buffer.value.clone(), inactive_style));
        }

        if !hint.is_empty() {
            spans.push(Span::styled(hint.to_string(), hint_style));
        }

        Line::from(spans)
    }

    /// The hint after the name field: a placeholder while it is empty, else
    /// whether this name is already registered.
    fn name_hint(&self) -> String {
        if self.name.value.trim().is_empty() {
            "  (e.g. local-clef)".to_string()
        } else if self.is_existing() {
            "  (updates this connection)".to_string()
        } else {
            String::new()
        }
    }

    /// The hint after the endpoint field: a placeholder while empty, else a
    /// note when the URL is a bare host, since the System One path is then
    /// appended for the request.
    fn base_url_hint(&self) -> String {
        let url = self.base_url.value.trim();
        if url.is_empty() {
            "  (e.g. http://localhost:11434)".to_string()
        } else if has_scheme(url) && has_host(url) && !has_path(url) {
            "  (the /v1/systemone path is added)".to_string()
        } else {
            String::new()
        }
    }

    fn model_hint(&self) -> String {
        if self.model.value.trim().is_empty() {
            "  (e.g. clef)".to_string()
        } else {
            String::new()
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        let popup = centered_rect(64, 46, area);
        popup::dialog(frame, popup, "Decision Provider", dimmed, |inner, buf| {
            let lines = vec![
                self.field_line(
                    "Name",
                    &self.name,
                    DecisionFormField::Name,
                    &self.name_hint(),
                ),
                self.field_line(
                    "Endpoint URL",
                    &self.base_url,
                    DecisionFormField::BaseUrl,
                    &self.base_url_hint(),
                ),
                self.field_line(
                    "API key",
                    &self.api_key,
                    DecisionFormField::ApiKey,
                    "  (optional — local servers need none)",
                ),
                self.field_line(
                    "Model",
                    &self.model,
                    DecisionFormField::Model,
                    &self.model_hint(),
                ),
            ];
            let body = Paragraph::new(lines).wrap(Wrap { trim: false });
            let [body_area, note_area, error_area, hint_area] =
                Layout::vertical([Min(0), Length(2), Length(1), Length(1)]).areas(inner);
            body.render(body_area, buf);
            Paragraph::new(
                "Decision models are not enumerable, so the model name is free \
                 text and is verified on the first call.",
            )
            .fg(theme::text_muted())
            .wrap(Wrap { trim: false })
            .render(note_area, buf);
            if let Some(e) = &self.error {
                Paragraph::new(e.as_str())
                    .fg(theme::error())
                    .render(error_area, buf);
            }
            Paragraph::new(theme::help_line(&[
                ("Tab", "next"),
                ("Enter", "save"),
                ("Esc", "cancel"),
            ]))
            .fg(theme::text_muted())
            .render(hint_area, buf);
        });
    }
}

/// Whether the URL names a scheme, i.e. can be handed to an HTTP client as-is.
fn has_scheme(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Whether anything follows the authority. A bare host root gains the System
/// One path when the request is built; a URL that already has one is used as
/// written, which is how Cloudflare's Workers AI route is configured.
///
/// Mirrors the request builder, which strips trailing slashes before it looks:
/// a bare host root written `http://host/` still gains the path, so the hint
/// must not read that slash as a path of its own.
fn has_path(url: &str) -> bool {
    let url = url.trim_end_matches('/');
    let authority_end = url.find("://").map_or(0, |index| index + "://".len());
    url.get(authority_end..)
        .is_some_and(|rest| rest.contains('/'))
}

/// Whether the URL names a host. `http://` alone satisfies [`has_scheme`] but
/// resolves to nothing.
fn has_host(url: &str) -> bool {
    let authority_end = url.find("://").map_or(0, |index| index + "://".len());
    url.get(authority_end..)
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| !host.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> AddDecisionProviderForm {
        AddDecisionProviderForm::new(&["existing".to_string()])
    }

    fn type_into(form: &mut AddDecisionProviderForm, field: DecisionFormField, value: &str) {
        form.field = field;
        for c in value.chars() {
            form.update(AddDecisionProviderMessage::Input(c));
        }
    }

    /// Fill every field with a valid connection.
    fn filled() -> AddDecisionProviderForm {
        let mut form = form();
        type_into(&mut form, DecisionFormField::Name, "local-clef");
        type_into(
            &mut form,
            DecisionFormField::BaseUrl,
            "http://localhost:11434",
        );
        type_into(&mut form, DecisionFormField::Model, "clef");
        form
    }

    #[test]
    fn submit_reports_each_required_field() {
        let cases = [
            (form(), "Name is required"),
            (
                {
                    let mut form = form();
                    type_into(&mut form, DecisionFormField::Name, "a");
                    form
                },
                "Endpoint URL is required",
            ),
            (
                {
                    let mut form = form();
                    type_into(&mut form, DecisionFormField::Name, "a");
                    type_into(&mut form, DecisionFormField::BaseUrl, "localhost:11434");
                    form
                },
                "Endpoint URL must start with http:// or https://",
            ),
            (
                {
                    let mut form = form();
                    type_into(&mut form, DecisionFormField::Name, "a");
                    type_into(&mut form, DecisionFormField::BaseUrl, "http://");
                    form
                },
                "Endpoint URL must name a host",
            ),
            (
                {
                    let mut form = form();
                    type_into(&mut form, DecisionFormField::Name, "a");
                    type_into(
                        &mut form,
                        DecisionFormField::BaseUrl,
                        "http://localhost:11434",
                    );
                    form
                },
                "Model is required",
            ),
        ];
        for (mut form, expected) in cases {
            assert_eq!(
                form.update(AddDecisionProviderMessage::Submit),
                AddDecisionProviderOutcome::None
            );
            assert_eq!(form.error.as_deref(), Some(expected));
        }
    }

    #[test]
    fn submit_trims_values_and_omits_a_blank_key() {
        let mut form = filled();
        type_into(&mut form, DecisionFormField::BaseUrl, "  ");
        // Re-fill, keeping the whitespace around a real URL.
        form.base_url.set("  http://localhost:11434  ");
        form.api_key.set("   ");

        assert_eq!(
            form.update(AddDecisionProviderMessage::Submit),
            AddDecisionProviderOutcome::Submit {
                name: "local-clef".to_string(),
                base_url: "http://localhost:11434".to_string(),
                api_key: None,
                model: "clef".to_string(),
            }
        );
    }

    #[test]
    fn submit_keeps_a_configured_key() {
        let mut form = filled();
        form.api_key.set(" secret ");
        assert_eq!(
            form.update(AddDecisionProviderMessage::Submit),
            AddDecisionProviderOutcome::Submit {
                name: "local-clef".to_string(),
                base_url: "http://localhost:11434".to_string(),
                api_key: Some("secret".to_string()),
                model: "clef".to_string(),
            }
        );
    }

    /// A model name is opaque: nothing about it is validated here, because
    /// nothing can be. `jev-latest`, an Ollama tag, and a Cloudflare route all
    /// have to round-trip untouched.
    #[test]
    fn submit_accepts_any_non_empty_model_name() {
        for model in ["clef-flash", "jev-latest", "kev", "@cf/cloudflare/clef"] {
            let mut form = filled();
            form.model.set(model);
            assert_eq!(
                form.update(AddDecisionProviderMessage::Submit),
                AddDecisionProviderOutcome::Submit {
                    name: "local-clef".to_string(),
                    base_url: "http://localhost:11434".to_string(),
                    api_key: None,
                    model: model.to_string(),
                }
            );
        }
    }

    #[test]
    fn fields_cycle_in_both_directions() {
        let mut form = form();
        assert_eq!(form.field, DecisionFormField::Name);
        for expected in [
            DecisionFormField::BaseUrl,
            DecisionFormField::ApiKey,
            DecisionFormField::Model,
            DecisionFormField::Name,
        ] {
            form.update(AddDecisionProviderMessage::NextField);
            assert_eq!(form.field, expected);
        }
        form.update(AddDecisionProviderMessage::PrevField);
        assert_eq!(form.field, DecisionFormField::Model);
    }

    #[test]
    fn typing_lands_in_the_focused_field() {
        let mut form = form();
        type_into(&mut form, DecisionFormField::BaseUrl, "abc");
        form.field = DecisionFormField::Name;
        form.update(AddDecisionProviderMessage::Input('z'));

        assert_eq!(form.name.value, "z");
        assert_eq!(form.base_url.value, "abc");
        assert_eq!(form.model.value, "");
    }

    #[test]
    fn prefill_seeds_every_field() {
        let form = form().prefill(
            "typesafe",
            Some("https://api.typesafe.ai"),
            Some("secret"),
            "jev-latest",
        );
        assert_eq!(form.name.value, "typesafe");
        assert_eq!(form.base_url.value, "https://api.typesafe.ai");
        assert_eq!(form.api_key.value, "secret");
        assert_eq!(form.model.value, "jev-latest");
        assert!(!form.is_existing());
    }

    /// The dialog is the only way to change a registered connection, so a
    /// seeded name that is already known has to read as an update.
    #[test]
    fn a_seeded_registered_name_reads_as_an_update() {
        let form = form().prefill("existing", Some("http://localhost:11434"), None, "clef");
        assert!(form.is_existing());
        assert_eq!(form.name_hint(), "  (updates this connection)");
    }

    #[test]
    fn the_endpoint_hint_tracks_whether_a_path_is_present() {
        let mut form = form();
        assert_eq!(form.base_url_hint(), "  (e.g. http://localhost:11434)");

        form.base_url.set("http://localhost:11434");
        assert_eq!(form.base_url_hint(), "  (the /v1/systemone path is added)");

        // The request builder strips a trailing slash before it decides, so a
        // bare host root written this way still gains the path.
        form.base_url.set("http://localhost:11434/");
        assert_eq!(form.base_url_hint(), "  (the /v1/systemone path is added)");

        // A complete URL, such as Cloudflare's Workers AI route, is used as
        // written — so no note about an appended path.
        form.base_url.set("https://api.typesafe.ai/v1/systemone");
        assert_eq!(form.base_url_hint(), "");

        // Nothing is claimed about a malformed URL before submission rejects it.
        for malformed in ["localhost:11434", "http://"] {
            form.base_url.set(malformed);
            assert_eq!(form.base_url_hint(), "", "{malformed}");
        }
    }

    #[test]
    fn cancel_is_reported_without_touching_the_fields() {
        let mut form = filled();
        assert_eq!(
            form.update(AddDecisionProviderMessage::Cancel),
            AddDecisionProviderOutcome::Cancel
        );
        assert_eq!(form.name.value, "local-clef");
    }

    #[test]
    fn escape_maps_to_cancel_and_enter_to_submit() {
        let form = form();
        let key = |code| KeyEvent::new(code, termina::event::Modifiers::NONE);
        assert_eq!(
            form.map_event(&key(KeyCode::Escape)),
            Some(AddDecisionProviderMessage::Cancel)
        );
        assert_eq!(
            form.map_event(&key(KeyCode::Enter)),
            Some(AddDecisionProviderMessage::Submit)
        );
        assert_eq!(
            form.map_event(&key(KeyCode::Tab)),
            Some(AddDecisionProviderMessage::NextField)
        );
    }
}
