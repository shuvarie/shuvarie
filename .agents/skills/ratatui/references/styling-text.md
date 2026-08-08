# Text and Styling

## The text hierarchy: `Text` / `Line` / `Span`

- `Text` — a list of `Line`s, plus an optional `style` and `alignment` applied to all lines.
- `Line` — a list of `Span`s (or `&str`/`String` via `Line::raw`), plus a `style`, `alignment`, and a trailing `LineEnding` (`Unix`/`Windows`/`NoEnding`).
- `Span` — a single run of text with one `Style`. The unit of styled text.

```rust
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};

let line = Line::from(vec![
    Span::raw("Hello "),
    Span::styled(
        "World",
        Style::new().fg(Color::Green).bg(Color::White).add_modifier(Modifier::BOLD),
    ),
    "!".red().on_light_yellow().italic(),
]);

let text = Text::from(line).style(Style::new().gray());
```

Construct from strings:

- `Text::raw(s)` / `Text::from(s)` — plain text, splits on `\n` into lines.
- `Line::raw(s)` — one line, single `Span` with no style.
- `Span::raw(s)` / `Span::styled(s, style)`.

`Paragraph::new(...)` accepts `&str`, `String`, `Line`, `Span`, or `Text`; the `Into` conversions handle it.

## `Style`

`Style` is a struct of optional fields: `fg`, `bg`, `underline_color`, `add_modifier`, `sub_modifier`. Build with the builder:

```rust
Style::new()
    .fg(Color::Red)
    .bg(Color::Black)
    .underline_color(Color::Blue)   // requires underline-color feature (on here with Termina)
    .add_modifier(Modifier::BOLD | Modifier::ITALIC)
    .sub_modifier(Modifier::DIM)
```

- `Style::new()` — empty (no-op).
- `Style::default()` — same.
- `.fg(Color)`, `.bg(Color)`, `.underline_color(Color)`.
- `.add_modifier(Modifier)`, `.remove_modifier(Modifier)` (alias `sub_modifier`).
- Styles **merge**: later styles override earlier ones field-by-field. `base.patch(other)` returns a combined style; `add_modifier`/`sub_modifier` are set-bitwise-merged.
- `reset()` — fully reset to defaults (emit a reset SGR).

## `Color`

| Group | Variants |
|-------|----------|
| Named | `Black`, `Red`, `Green`, `Yellow`, `Blue`, `Magenta`, `Cyan`, `Gray` (+ light_ variants `LightRed` etc.), `DarkGray` |
| Indexed | `Color::Indexed(u8)` — 256-color palette |
| RGB | `Color::Rgb(r, g, b)` — true color |
| Reset | `Color::Reset` — use terminal default |

With the `palette` feature (off here), convert `palette::Srgb`/`Hsl`/`...` into `Color::Rgb`.

## `Modifier` (bitflags)

`BOLD`, `DIM`, `ITALIC`, `UNDERLINED`, `REVERSED`, `CROSSED_OUT`, `SLOW_BLINK`, `RAPID_BLINK`, `HIDDEN`, `ENCIRCLED`, `OVERLINED`, `RAPID_BLINK`. Combine with `|`.

## `Stylize` — shorthand syntax

The `Stylize` trait (in `prelude`) enables the fluent shorthand on `&str`, `String`, `Span`, `Line`, `Text`, and widgets:

```rust
"error".red().bold();
"warn".yellow();
"ok".green().on_black();
text.underlined().bg(Color::Blue);
line.alignment(Alignment::Center);
paragraph.style(Style::new().red().on_white());
```

- Foreground: `.red()`, `.green()`, `.blue()`, `.yellow()`, `.cyan()`, `.magenta()`, `.gray()`, `.dark_gray()`, `.light_red()`, `.light_green()`, `.light_blue()`, `.light_yellow()`, `.light_magenta()`, `.light_cyan()`, `.white()`, `.black()`.
- Background: `.on_red()`, `.on_green()`, `.on_blue()`, `.on_yellow()`, `.on_cyan()`, `.on_magenta()`, `.on_white()`, `.on_black()`, `.on_light_*`, `.on_dark_gray()`.
- Modifiers: `.bold()`, `.dim()`, `.italic()`, `.underlined()`, `.reversed()`, `.crossed_out()`, `.slow_blink()`, `.rapid_blink()`, `.hidden()`, `.encircled()`, `.overlined()`, `.not_bold()` / `.reset_*()` to undo.
- `.bg(Color)` / `.fg(Color)` accept any `Color` (e.g. `.fg(Color::Rgb(0, 0, 0))`).
- `.style(Style)` replaces the entire style.

## Alignment

`Alignment::Left` / `Alignment::Center` / `Alignment::Right`. Set on `Line` or `Paragraph`:

```rust
Paragraph::new("centered").alignment(Alignment::Center);
Line::from("centered").centered();
```

## Wrapping and scroll (Paragraph)

- `Paragraph::wrap(Wrap { trim: bool })` — word-wrap; `trim` strips leading whitespace on wrapped lines.
- `Paragraph::scroll((rows, cols))` — offset content; useful for chat/log views.
- No `line_count`/`line_width` (behind `unstable-rendered-line-info`, off here) — compute scroll bounds yourself by tracking lines.

## Styling a whole widget vs. its text

```rust
Paragraph::new("Hello World!").style(Style::new().red().on_white());   // applies to the whole widget area
Paragraph::new("Hello World!".red().on_white().bold());                 // applies to the text runs only
```

Prefer `.style(...)` for area-wide styling (background fill) and per-`Span`/`Line` styling for content.

## Applying styles to widgets

Most widgets take `.style(Style)` and `.highlight_style(Style)`:

- `Block::style(...)`, `Block::border_style(...)`, `Block::title_style(...)`.
- `List::highlight_style(...)`, `List::style(...)`.
- `Table::row_highlight_style(...)`, `Table::style(...)`, `Cell::style(...)`.
- `Gauge::gauge_style(...)`, `BarChart::bar_style(...)`, etc.

## Common patterns

### Colored label + value

```rust
Line::from(vec![
    Span::styled("name: ", Style::new().cyan()),
    Span::raw(&self.name),
])
```

### Error line

```rust
format!("error: {err}").red().bold();
```

### Dimmed hint

```rust
"press q to quit".dark_gray();
```

### Reversed selection

```rust
List::new(items).highlight_style(Style::new().reversed());
```

### Status bar with mixed styles

```rust
Line::from(vec![
    Span::raw(" "),
    Span::styled("●", Style::new().fg(Color::Green)),
    Span::raw(" connected "),
    Span::styled(model_name, Style::new().bold()),
])
```

## String lifetimes

`Span`/`Line`/`Text` can borrow `&str` or own `String`. For values that must outlive the frame (e.g. computed per draw), build from owned `String` or use `Span::raw` with `&str` whose lifetime is the model. Ratatui's `Buffer` copies the grapheme into its cells during render, so the styled text only needs to live until the `draw` closure returns.