# Widgets and Layout

## Layout — `Layout`, `Constraint`, `Rect`

Ratatui splits a `Rect` into sub-areas with `Layout`. Call `.areas(rect)` (returns `[Rect; N]` matching the number of constraints) or `.split(rect)` (returns `Rc<[Rect]>` for dynamic-length use).

```rust
use ratatui::layout::{Constraint, Layout, Rect};
use Constraint::{Fill, Length, Min, Percentage, Ratio};

let vertical = Layout::vertical([Length(1), Min(0), Length(1)]);
let [title, main, status] = vertical.areas(frame.area());

let horizontal = Layout::horizontal([Fill(1); 2]);
let [left, right] = horizontal.areas(main);
```

### Constraint variants

| Variant | Meaning |
|---------|---------|
| `Length(u16)` | Fixed number of cells |
| `Length(u16)` | Fixed number of cells |
| `Fill(u16)` | Grow to fill remaining space (weight) |
| `Min(u16)` | At least N cells, grows to fill |
| `Max(u16)` | At most N cells |
| `Percentage(u16)` | Percentage of available space (0–100) |
| `Ratio(u32, u32)` | Proportional split (e.g. `Ratio::new(1, 2)`) |
| `Spacing(u16)` | Empty gutter (used with `Layout::spacing`) |

### Layout methods

- `Layout::vertical(constraints)` / `Layout::horizontal(constraints)` — direction.
- `.areas(rect)` — returns `[Rect; N]` (compile-time-checked length match). Panics if lengths differ.
- `.split(rect)` — returns `Rc<[Rect]>` for runtime-length splits.
- `.spacing(n)` — insert `n` empty cells between constraints.
- `.margin(n)` — inset all sub-areas by `n`.
- `.constraints(...)` — set constraints (builder style).
- `layout-cache` feature (on here) — caches layout results for speed.

### Nesting

Compose layouts by splitting recursively:

```rust
let [left, right] = Layout::horizontal([Fill(1); 2]).areas(frame.area());
let [top, bottom] = Layout::vertical([Length(3), Min(0)]).areas(left);
```

### `Rect`

A plain rectangle: `x`, `y`, `width`, `height` (all `u16`). Helpers: `.area()` (cells count), `.contains(other)`, `.splitting(...)`, arithmetic via `Rect` methods. `frame.area()` returns the full drawable `Rect`.

## Widget catalog

All widgets live under `ratatui::widgets::*` (re-exported from `ratatui-widgets`). Render with `frame.render_widget(widget, area)` or `frame.render_stateful_widget(widget, area, &mut state)`.

### `Block` — bordered/titled container

```rust
use ratatui::widgets::{Block, Borders, BorderType, Padding};

let block = Block::bordered()
    .title("Title")
    .title_alignment(Alignment::Center)
    .border_type(BorderType::Rounded)
    .borders(Borders::TOP | Borders::BOTTOM)
    .padding(Padding::uniform(1));
frame.render_widget(block, area);
```

- `Block::new()` — no borders.
- `Block::bordered()` — all four borders.
- `.title(Line)` / `.title_top(Line)` / `.title_bottom(Line)` — titles accept `&str`, `Line`, or `Title` (with alignment).
- `.borders(Borders::LEFT | ...)` — selective borders (the `border!` macro composes these).
- `.border_type(BorderType::Plain|Rounded|Double|Thick|QuadrantInside|...)`.
- `.padding(Padding::new(left, right, top, bottom))` / `Padding::uniform(n)`.
- `Block` is often a `.inner(area)` container: pass the inner area to a child widget.

### `Paragraph` — multi-line text

```rust
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::layout::Alignment;

let para = Paragraph::new(text)
    .alignment(Alignment::Left)
    .wrap(Wrap { trim: true })
    .scroll((offset_lines, 0));   // vertical scroll
frame.render_widget(para, area);
```

- `.wrap(Wrap { trim: bool })` — enable word wrap; `trim` strips leading whitespace on wrapped lines.
- `.scroll((rows, cols))` — scroll the content; useful for chat/log views.
- `.alignment(Alignment::Left|Center|Right)`.
- `line_count` / `line_width` are behind `unstable-rendered-line-info` (off here) — don't use them.

### `List` / `ListState` — selectable list (stateful)

```rust
use ratatui::widgets::{List, ListItem, ListState};

let items = ["Apple", "Banana", "Cherry"].iter().map(|s| ListItem::new(*s));
let list = List::new(items)
    .block(Block::bordered().title("Fruits"))
    .highlight_symbol("▶ ")
    .highlight_style(Style::new().yellow().bold())
    .repeat_highlight_symbol(false);

let mut state = ListState::default();
state.select(Some(1));
frame.render_stateful_widget(list, area, &mut state);
```

- `ListState::select(Some(idx))` / `selected()`.
- Navigation helpers (Ratatui 0.30): `state.select_next()`, `state.select_previous()`, `state.select_first()`, `state.select_last()`, `state.offset()`, `state.select_index(n)`.
- `ListItem::new(content)` where content is `&str`, `Line`, or `Text`.
- `.highlight_symbol`, `.highlight_style`, `.highlight_spacing` control the selection marker.

### `Table` / `Row` / `Cell` — tabular data

```rust
use ratatui::widgets::{Row, Table};

let rows = [
    Row::new(vec!["Alice", "30", "Engineer"]),
    Row::new(vec!["Bob", "25", "Designer"]),
];
let table = Table::new(rows, [Constraint::Length(10), Constraint::Length(5), Constraint::Min(10)])
    .header(Row::new(vec!["Name", "Age", "Role"]).style(Style::new().bold()))
    .block(Block::bordered().title("People"))
    .row_highlight_style(Style::new().reversed())
    .widths([Constraint::Min(5); 3]);
frame.render_widget(table, area);
```

- `Row::new(cells)`, `Cell::new(content).style(...)`.
- Widths via constraints in `Table::new(rows, widths)` or `.widths(...)`.
- `.header(Row)`, `.footer(Row)`.
- Stateful selection uses `TableState` (same shape as `ListState`).

### `Tabs` — tab strip (stateful)

```rust
use ratatui::widgets::Tabs;

let tabs = Tabs::new(["Chat", "Models", "Settings"])
    .block(Block::bordered().title("Menu"))
    .highlight_style(Style::new().yellow().bold())
    .select(active_tab_idx)
    .divider("|");
frame.render_widget(tabs, area);
```

### `Gauge` / `BarChart` — progress and bars

```rust
use ratatui::widgets::{Gauge, BarChart, Bar};

let gauge = Gauge::default()
    .block(Block::bordered().title("Progress"))
    .gauge_style(Style::new().blue())
    .percent(75);

let chart = BarChart::default()
    .block(Block::bordered().title("Usage"))
    .data(&[("CPU", 75), ("MEM", 40)])
    .bar_width(5)
    .bar_gap(2)
    .bar_style(Style::new().green());
```

- `Gauge::percent(0..=100)` or `Gauge::ratio(0.0..=1.0)`.
- `BarChart::data(&[(&str, u64)])`, `.bar_style(...)`, `.value_style(...)`, `.label_style(...)`.

### `Sparkline` — inline mini-chart

```rust
use ratatui::widgets::Sparkline;
let spark = Sparkline::default().data(&[1, 3, 2, 4, 5, 3]).max(5);
```

### `Canvas` — custom drawing

```rust
use ratatui::widgets::{Canvas, canvas::*};

let canvas = Canvas::default()
    .block(Block::bordered().title("Map"))
    .marker(Marker::Braille)
    .paint(|ctx| {
        ctx.draw(&Map { resolution: 40, color: Color::White });
        ctx.print(0.0, 0.0, "x");
    })
    .x_bounds([-180.0, 180.0])
    .y_bounds([-90.0, 90.0]);
```

Use `Canvas` for free-form drawing with points/lines/shapes mapped onto a world coordinate system. Markers: `Marker::Braille` (highest resolution), `Marker::HalfBlock`, `Marker::Bar`, `Marker::Block`.

### `Clear` — clear an area

`frame.render_widget(Clear, area);` blanks a region (useful for popups drawn over existing content).

### `Scrollbar` / `ScrollbarState`

Decorative scrollbar; pair with a separate scroll offset on `Paragraph`/`List`:

```rust
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
let mut sb_state = ScrollbarState::new(content_len).position(pos);
frame.render_stateful_widget(
    Scrollbar::new(ScrollbarOrientation::VerticalRight),
    area,
    &mut sb_state,
);
```

### `Calendar` (feature `widget-calendar`, on here)

Month grid widget. Requires a `chrono` or `time` backend; see the `ratatui::widgets::calendar` module if needed.

## Stateful widgets — pattern

Stateful widgets keep their selection/offset in a separate `*State` struct the app owns. Pass `&mut state` to `render_stateful_widget`. Keep the state in your `App` (or TUI model) so it persists across frames:

```rust
pub struct App {
    list_state: ListState,
    scroll_offset: u16,
}
```

This matches the project's TEA model: state lives in `App`, `view` borrows it for rendering, `update` mutates it on messages.

## Custom widgets

Implement the `Widget` trait:

```rust
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

struct HelloWorld;

impl Widget for HelloWorld {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // write cells into buf at (area.x, area.y, ...)
        let cell = buf.cell_mut((area.x, area.y)).unwrap();
        cell.set_char('H');
    }
}
```

For stateful custom widgets implement `StatefulWidget` (takes `&mut State`). For by-reference rendering (needed by some container widgets) enable `unstable-widget-ref` and implement `WidgetRef` — off in this project, so prefer owning the widget for rendering.

## `prelude` import

`use ratatui::prelude::*;` brings in `Frame`, `Layout`/`Constraint`/`Rect`/`Direction`/`Alignment`/`Margin`, `Style`/`Color`/`Modifier`/`Stylize`, `Text`/`Line`/`Span`, `Buffer`/`Cell`, `Widget`, `Backend`, and the backend structs. Add explicit `use ratatui::widgets::{...};` for specific widgets.