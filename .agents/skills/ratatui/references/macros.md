# ratatui-macros

The `macros` feature (enabled in this project) re-exports `ratatui-macros` 0.7.x as `ratatui::macros`. The macros construct common types tersely. They are optional sugar — the regular builders do the same thing.

## Import

```rust
use ratatui::macros::*;
// or directly: ratatui::macros::line!, span!, text!, layout!, rect!, buffer!
```

## `line!` — build a `Line`

```rust
use ratatui::macros::line;
use ratatui::style::Stylize;

let l = line!["Hello ", "World".red().bold()];
// equivalent to Line::from(vec![Span::raw("Hello "), Span::styled("World", ...)])
```

Each element can be `&str`/`String` (→ `Span::raw`) or a `Span`. Use `,` between spans. Trailing comma optional.

## `span!` — build a `Span`

```rust
use ratatui::macros::span;
use ratatui::style::{Color, Style};

let s = span!("error".red().bold());
let s = span!("text", Style::new().fg(Color::Red));
```

## `text!` — build `Text`

```rust
use ratatui::macros::text;
use ratatui::style::Stylize;

let t = text![
    "Title".bold(),
    "",
    "Body line one",
    "Body line two".dim(),
];
```

Each element becomes a `Line`; `&str`/`String` become `Line::raw`, and `Line`/`Span` pass through. Blank `""` lines become empty lines.

## `layout!` — build a `Layout`

```rust
use ratatui::macros::layout;
use ratatui::layout::Constraint;

let layout = layout![
    Constraint::Length(3),
    Constraint::Min(0),
    Constraint::Length(1),
];
// a vertical Layout with three constraints — call .areas(rect) / .split(rect) on it

let [top, mid, bottom] = layout.areas(frame.area());
```

The macro returns a `Layout` (vertical by default). Pass `Constraint` values positionally.

## `rect!` — build a `Rect`

```rust
use ratatui::macros::rect;
let r = rect![0, 0, 80, 24];   // x, y, width, height
```

## `buffer!` — build a `Buffer`

```rust
use ratatui::macros::buffer;
let buf = buffer![80, 24];   // empty buffer of that size
```

## `border!` — compose `Borders`

The `border!` macro (also re-exported at the crate root as `ratatui::border!`) composes a `Borders` bitflag from side names:

```rust
use ratatui::widgets::{Block, Borders};
use ratatui::border;

let b = Block::new().borders(border![top, bottom]);
let all = Block::bordered().borders(border![all]);   // all four sides
```

Sides: `top`, `bottom`, `left`, `right`, `all`. Combine with `,`.

## When to use macros

- Tightening test/code where readability benefits (e.g. multi-line `text![...]` for help text).
- Static layouts in `view` where `layout![Length(1), Min(0), Length(1)]` reads better than `Layout::vertical([Length(1), Min(0), Length(1)])`.

Prefer the explicit builders when:
- The constraints depend on runtime values that don't fit a comma list cleanly.
- Clippy or readability flags the macro form.
- You need `.spacing()`, `.margin()`, or other builder methods on the layout.

## Macro availability

All these macros require the `macros` feature on `ratatui` (on in this project). They live in the `ratatui::macros` module. The `border!` macro is also available directly at `ratatui::border!`.