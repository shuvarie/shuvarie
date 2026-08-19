use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Rgb(212, 175, 95);
pub const TEXT: Color = Color::Rgb(224, 216, 196);
pub const TEXT_DIM: Color = Color::Rgb(128, 120, 104);
pub const TEXT_MUTED: Color = Color::Rgb(92, 86, 74);
pub const SURFACE: Color = Color::Rgb(28, 28, 34);
pub const ACCENT_BG: Color = Color::Rgb(52, 48, 42);
pub const SUCCESS: Color = Color::Rgb(138, 146, 90);
pub const ERROR: Color = Color::Rgb(186, 88, 72);
pub const WARNING: Color = Color::Rgb(192, 152, 72);

pub const STEEL: Color = Color::Rgb(148, 160, 204);
pub const SAGE: Color = Color::Rgb(160, 176, 118);
pub const BRONZE: Color = Color::Rgb(176, 158, 122);
pub const AMBER: Color = Color::Rgb(192, 152, 72);
pub const GULES: Color = Color::Rgb(186, 88, 72);

pub const PLAIN: Style = Style::new().fg(TEXT);
pub const KEYWORD: Style = Style::new().fg(ACCENT);
pub const STRING: Style = Style::new().fg(SAGE);
pub const COMMENT: Style = Style::new().fg(TEXT_DIM).add_modifier(Modifier::ITALIC);
pub const CONSTANT: Style = Style::new().fg(AMBER);
pub const TYPE: Style = Style::new().fg(STEEL);
pub const FUNCTION: Style = Style::new().fg(BRONZE);
pub const INVALID: Style = Style::new().fg(GULES);
