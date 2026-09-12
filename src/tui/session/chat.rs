use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use ratatui::prelude::*;
use serde_json::Value;
use shuvarie_core::tool_record::ToolRecord;
use shuvarie_core::{DiagnosticInfo, Role};
use shuvarie_db::ReasoningSegment;
use shuvarie_llm::{FileChange, ShellStreams};
use unicode_width::UnicodeWidthStr;

use super::MouseKind;
use super::blocks::{
    Block, BlockMessage, ChatEnv, ContextBlock, ReasoningBlock, SteeredPrompt, SystemText,
    TextBlock, ToolBlock, ToolMessage, UserPrompt,
};
use super::segment::{BlockAddr, ResolvedRow, slice_visual, visual_row_text};
use super::virtualizer::{TurnData, TurnEst, TurnFlags, locate, paint_turn};
use crate::tui::theme;

/// Selection column bounds of one wrapped row.
type WrapBounds = (Option<u16>, Option<u16>);

/// One logical source row under the selection: which turn/segment/source
/// row it came from, the wrapped rows it covers with their selection column
/// bounds, and the resolved handle for extraction.
struct CopyGroup {
    key: (usize, usize, u32),
    wraps: Vec<(u32, WrapBounds)>,
    resolved: ResolvedRow,
}

/// Drop selection column bounds that touch a row's text edges: a selection
/// starting at the text's first column or ending past its last column still
/// copies the whole logical row cleanly.
fn normalize_cols(
    resolved: &ResolvedRow,
    from: Option<u16>,
    to: Option<u16>,
) -> (Option<u16>, Option<u16>) {
    let end = resolved.pad_x.saturating_add(resolved.text_width);
    (
        from.filter(|c| *c > resolved.pad_x),
        to.filter(|c| *c < end),
    )
}

impl CopyGroup {
    fn flush(self, out: &mut Vec<Option<String>>) {
        let total = self.resolved.wrap_total as usize;
        let full = self.wraps.len() == total
            && self.wraps.iter().enumerate().all(|(i, (w, bounds))| {
                *w as usize == i && bounds.0.is_none() && bounds.1.is_none()
            });
        if full {
            out.push(Some(self.resolved.copy_text()));
            return;
        }
        let line = self.resolved.line();
        for (w, (from, to)) in &self.wraps {
            let text = visual_row_text(&line, self.resolved.text_width, self.resolved.trim, *w);
            let text = if from.is_some() || to.is_some() {
                let a = from.map_or(0, |c| usize::from(c.saturating_sub(self.resolved.pad_x)));
                let b = to.map_or(usize::MAX, |c| {
                    usize::from(c.saturating_sub(self.resolved.pad_x))
                });
                slice_visual(&text, a, b)
            } else {
                text
            };
            out.push(Some(text));
        }
    }
}

/// Selection column bounds for one highlighted content row: interior rows
/// span the full content width, boundary rows run from the selection's
/// column, inset by the segment's text padding.
fn sel_columns(
    turn: usize,
    row: u32,
    start: &SelPos,
    end: &SelPos,
    pad: u16,
    content_width: u16,
) -> (u16, u16) {
    let from = (turn == start.turn && start.row == row).then_some(start.col);
    let to = (turn == end.turn && end.row == row).then_some(end.col);
    let x0 = from.map_or(0, |c| pad.saturating_add(c).min(content_width));
    let x1 = to.map_or(content_width, |c| pad.saturating_add(c).min(content_width));
    (x0, x1)
}

/// Paint the selection background over `[x0, x1)` of one row, extending a
/// boundary by one cell when a wide glyph straddles it.
fn tint_row(buf: &mut Buffer, clip: Rect, content_width: u16, y: u16, mut x0: u16, mut x1: u16) {
    let right = clip.x.saturating_add(content_width);
    x0 = x0.min(right);
    x1 = x1.min(right);
    if x0 >= x1 {
        return;
    }
    if x0 > clip.x && buf[(x0 - 1, y)].symbol().width() == 2 {
        x0 -= 1;
    }
    if x1 < right && buf[(x1 - 1, y)].symbol().width() == 2 {
        x1 += 1;
    }
    for x in x0..x1 {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_bg(theme::SELECTION);
        }
    }
}

pub enum ChatMessage {
    BeginUserTurn {
        content: String,
    },
    TokenReceived {
        content: String,
    },
    ReasoningReceived {
        content: String,
    },
    ContextLoaded {
        paths: Vec<String>,
    },
    ToolStarted {
        name: String,
        args: Value,
        worker: Option<String>,
        call_id: Option<String>,
    },
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
        worker: Option<String>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
        duration_ms: u64,
        call_id: Option<String>,
    },
    ToolOutput {
        tool: String,
        worker: Option<String>,
        stdout: String,
        stderr: String,
    },
    WorkerStarted {
        name: String,
        args: Value,
        call_id: Option<String>,
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
        duration_ms: u64,
        call_id: Option<String>,
    },
    StreamDone,
    StreamError {
        error: String,
    },
    StreamCancelled,
    Load {
        session: shuvarie_core::Session,
    },
    TurnReverted {
        session: shuvarie_core::Session,
    },
    TurnRestored {
        session: shuvarie_core::Session,
    },
    Reset,
    LspDiagnostics {
        path: String,
        diagnostics: Vec<DiagnosticInfo>,
    },
    ScrollUp,
    ScrollDown,
    /// Left-button mouse activity at a terminal cell inside the history
    /// pane: down starts a selection, drag extends it, up finalizes — or
    /// toggles the block under a click (movement under the drag slop).
    Mouse {
        kind: super::MouseKind,
        column: u16,
        row: u16,
    },
    Wheel {
        up: bool,
        column: u16,
        row: u16,
    },
    ToggleLastTool,
    /// A prompt was queued (steered) while the agent works.
    SteeredQueued {
        content: String,
    },
    /// The first queued prompt was dispatched as a new user turn.
    SteeredDispatched,
    /// The most recently queued prompt was recalled into the input area.
    SteeredRecalled,
    /// The queue was wiped by a session-level transition.
    SteeredCleared,
    /// The render loop's spinner wake: the turns that can hold animated
    /// blocks — the in-flight turn and a committed turn with a late
    /// still-running `ToolFinished` straggler — repaint their spinner-bearing
    /// segments in place without invalidating their caches.
    SpinnerUpdate,
}

/// Viewport-relative scroll position. `sticky_bottom` tracks the streaming
/// follow state: pinned to the bottom while true, released by any upward
/// scroll and re-engaged when scrolling back down to the last row.
#[derive(Debug, Default, Clone, Copy)]
struct Scroll {
    offset: u32,
    sticky_bottom: bool,
}

/// A selection endpoint in content space: turn index (same space as
/// [`BlockAddr::turn`]), the wrapped row within that turn, and the column
/// within the content width. Anchored, so it survives scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SelPos {
    turn: usize,
    row: u32,
    col: u16,
}

/// A drag selection: the fixed anchor and the moving head.
#[derive(Debug, Clone, Copy)]
struct SelRange {
    anchor: SelPos,
    head: SelPos,
}

/// Movement under this many cells (rows + columns) counts as a click, not a
/// drag, and toggles the block under the cursor on mouse-up.
const CLICK_SLOP: u32 = 3;

/// Clicks within this window at the same cell escalate: double selects the
/// word, triple the whole logical row.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(500);

impl SelRange {
    /// The endpoints ordered ascending; `None` for a collapsed (zero-width)
    /// selection.
    fn ordered(&self) -> Option<(SelPos, SelPos)> {
        let (a, b) = if (self.anchor.turn, self.anchor.row, self.anchor.col)
            <= (self.head.turn, self.head.row, self.head.col)
        {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        let collapsed = a.turn == b.turn && a.row == b.row && a.col >= b.col;
        (!collapsed).then_some((a, b))
    }

    /// Drag distance in content cells: rows plus columns; a cross-turn head
    /// is always a drag.
    fn drag_distance(&self) -> u32 {
        if self.anchor.turn != self.head.turn {
            return u32::MAX;
        }
        self.anchor
            .row
            .abs_diff(self.head.row)
            .saturating_add(u32::from(self.anchor.col.abs_diff(self.head.col)))
    }
}

/// The chat history pane: committed turns (lazily materialized TEA block
/// lists backed by the stored session), the in-flight streaming turn, and the
/// windowed scroll engine. Only turns intersecting the viewport (plus an
/// overscan band) render their segments; far-away turns are evicted back to
/// estimated lazy slots so long sessions scroll without full-content
/// rebuilds. Scroll position and the scrollbar derive from mixed exact and
/// estimated row heights.
pub struct Chat {
    turns: RefCell<Vec<TurnData>>,
    in_flight: RefCell<Option<TurnData>>,
    /// Queued (steered) prompts waiting to take over at the agent's next
    /// completed action (tool call, thinking, or text segment). Rendered as
    /// pseudo-turns pinned after the in-flight turn.
    steered: RefCell<Vec<TurnData>>,
    streaming: bool,
    interrupted: bool,
    lsp_diagnostics: BTreeMap<String, Vec<DiagnosticInfo>>,
    stored: Option<shuvarie_core::Session>,
    stored_len: usize,
    scroll: RefCell<Scroll>,
    width: Cell<u16>,
    env_rev: u64,
    toggled: BTreeSet<(usize, usize)>,
    /// Live drag selection in content space; survives scrolling.
    selection: RefCell<Option<SelRange>>,
    /// A mouse button is down inside the history pane.
    dragging: Cell<bool>,
    /// Last click for double/triple-click escalation: position, time, count.
    last_click: RefCell<Option<(SelPos, Instant, u8)>>,
    /// Set when mouse-up finalized a non-empty selection, for the
    /// copy-on-select hook the session consumes.
    pending_copy: Cell<bool>,
    history_rect: Cell<Rect>,
}

impl Chat {
    pub fn new() -> Self {
        Self {
            turns: RefCell::new(Vec::new()),
            in_flight: RefCell::new(None),
            steered: RefCell::new(Vec::new()),
            streaming: false,
            interrupted: false,
            lsp_diagnostics: BTreeMap::new(),
            stored: None,
            stored_len: 0,
            scroll: RefCell::new(Scroll {
                offset: 0,
                sticky_bottom: true,
            }),
            width: Cell::new(0),
            env_rev: 0,
            toggled: BTreeSet::new(),
            selection: RefCell::new(None),
            dragging: Cell::new(false),
            last_click: RefCell::new(None),
            pending_copy: Cell::new(false),
            history_rect: Cell::new(Rect::default()),
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Whether any steered prompts are queued (drives the recall help hint).
    pub fn has_steered(&self) -> bool {
        !self.steered.borrow().is_empty()
    }

    pub fn has_messages(&self) -> bool {
        !self.turns.borrow().is_empty()
    }

    pub fn last_turn_interrupted(&self) -> bool {
        self.interrupted
            && self
                .turns
                .borrow()
                .last()
                .is_some_and(|turn| turn.role == Role::Assistant)
    }

    /// Whether any tool block is still animating: live blocks in the
    /// in-flight turn or a straggler in the last committed turn. Only those
    /// turns can hold running blocks — committed turns are built from
    /// in-flight data or stored records, neither of which animates.
    pub fn has_running_tool_blocks(&self) -> bool {
        self.in_flight
            .borrow()
            .as_ref()
            .is_some_and(Self::turn_has_running_tool)
            || self
                .turns
                .borrow()
                .last()
                .is_some_and(Self::turn_has_running_tool)
    }

    fn turn_has_running_tool(turn: &TurnData) -> bool {
        turn.blocks
            .as_deref()
            .is_some_and(|blocks| blocks.iter().any(|block| block.tool_is_running()))
    }

    pub fn update(&mut self, msg: ChatMessage) {
        match msg {
            ChatMessage::BeginUserTurn { content } => {
                if self.in_flight.borrow().is_some() {
                    self.clear_selection_from(self.turns.borrow().len());
                }
                let mut turn = TurnData::new(Role::User);
                turn.set_blocks(vec![Block::User(UserPrompt::new(content))], false);
                self.turns.borrow_mut().push(turn);
            }
            ChatMessage::TokenReceived { content } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.append_text(content);
            }
            ChatMessage::ReasoningReceived { content } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.append_reasoning(content);
            }
            ChatMessage::ContextLoaded { paths } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.push_block(Block::Context(ContextBlock::new(paths)));
            }
            ChatMessage::ToolStarted {
                name,
                args,
                worker,
                call_id,
            } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.push_block(Block::Tool(Box::new(ToolBlock::new(
                    name,
                    args.to_string(),
                    worker,
                    call_id,
                ))));
            }
            ChatMessage::ToolFinished {
                name,
                ok,
                output,
                worker,
                file_change,
                streams,
                duration_ms,
                call_id,
            } => {
                let (display_output, display_stderr) = match streams {
                    Some(streams) => (streams.stdout, streams.stderr),
                    None => (output, String::new()),
                };
                self.with_running_tool(&name, &worker, call_id.as_deref(), |block| {
                    block.update(BlockMessage::Tool(ToolMessage::Finish {
                        ok,
                        output: display_output,
                        stderr: display_stderr,
                        file_change,
                        duration_ms,
                    }))
                });
                self.touch_in_flight();
            }
            ChatMessage::ToolOutput {
                tool,
                worker,
                stdout,
                stderr,
            } => {
                let updated = self.with_running_tool(&tool, &worker, None, |block| {
                    block.update(BlockMessage::Tool(ToolMessage::Output { stdout, stderr }))
                });
                if updated {
                    self.touch_in_flight();
                }
            }
            ChatMessage::WorkerStarted {
                name,
                args,
                call_id,
            } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.push_block(Block::Tool(Box::new(ToolBlock::new(
                    name,
                    args.to_string(),
                    Some(String::new()),
                    call_id,
                ))));
            }
            ChatMessage::WorkerFinished {
                name,
                ok,
                output,
                duration_ms,
                call_id,
            } => {
                self.with_running_tool(&name, &Some(String::new()), call_id.as_deref(), |block| {
                    block.update(BlockMessage::Tool(ToolMessage::Finish {
                        ok,
                        output,
                        stderr: String::new(),
                        file_change: None,
                        duration_ms,
                    }))
                });
                self.touch_in_flight();
            }
            ChatMessage::StreamDone => self.commit_done(),
            ChatMessage::StreamError { .. } | ChatMessage::StreamCancelled => {
                self.commit_interrupted()
            }
            ChatMessage::Load { session } => self.apply_session(session, true),
            ChatMessage::TurnReverted { session } => self.apply_session(session, true),
            ChatMessage::TurnRestored { session } => self.apply_session(session, false),
            ChatMessage::Reset => {
                *self.turns.borrow_mut() = Vec::new();
                *self.in_flight.borrow_mut() = None;
                self.steered.borrow_mut().clear();
                self.stored = None;
                self.stored_len = 0;
                self.streaming = false;
                self.interrupted = false;
                self.toggled.clear();
                self.clear_selection();
                let mut scroll = self.scroll.borrow_mut();
                scroll.offset = 0;
                scroll.sticky_bottom = true;
            }
            ChatMessage::LspDiagnostics { path, diagnostics } => {
                if diagnostics.is_empty() {
                    self.lsp_diagnostics.remove(&path);
                } else {
                    self.lsp_diagnostics.insert(path, diagnostics);
                }
                self.env_rev += 1;
            }
            ChatMessage::ScrollUp => {
                let mut scroll = self.scroll.borrow_mut();
                scroll.offset = scroll.offset.saturating_sub(1);
                scroll.sticky_bottom = false;
            }
            ChatMessage::ScrollDown => {
                let mut scroll = self.scroll.borrow_mut();
                scroll.offset = scroll.offset.saturating_add(1);
            }
            ChatMessage::Mouse { kind, column, row } => {
                self.handle_mouse(kind, column, row);
            }
            ChatMessage::Wheel { up, column, row } => {
                if self.in_history(column, row) {
                    let mut scroll = self.scroll.borrow_mut();
                    for _ in 0..3 {
                        if up {
                            scroll.offset = scroll.offset.saturating_sub(1);
                            scroll.sticky_bottom = false;
                        } else {
                            scroll.offset = scroll.offset.saturating_add(1);
                        }
                    }
                }
            }
            ChatMessage::ToggleLastTool => self.toggle_last_tool(),
            ChatMessage::SteeredQueued { content } => {
                let mut turn = TurnData::new(Role::User);
                turn.set_blocks(vec![Block::Steered(SteeredPrompt::new(content))], false);
                self.steered.borrow_mut().push(turn);
            }
            ChatMessage::SteeredDispatched => {
                let mut steered = self.steered.borrow_mut();
                if !steered.is_empty() {
                    steered.remove(0);
                    drop(steered);
                    self.remap_after_steered_removed();
                }
            }
            ChatMessage::SteeredRecalled => {
                self.steered.borrow_mut().pop();
                let removed = self.turns.borrow().len() + 1 + self.steered.borrow().len();
                self.clear_selection_from(removed);
            }
            ChatMessage::SteeredCleared => {
                self.steered.borrow_mut().clear();
                let from = self.turns.borrow().len() + 1;
                self.clear_selection_from(from);
            }
            ChatMessage::SpinnerUpdate => {
                let env = ChatEnv {
                    lsp_diagnostics: &self.lsp_diagnostics,
                    rev: self.env_rev,
                };
                if let Some(turn) = self.in_flight.get_mut() {
                    turn.refresh_spinners(&env);
                }
                if let Some(turn) = self.turns.get_mut().last_mut()
                    && turn.has_running_tool()
                {
                    turn.refresh_spinners(&env);
                }
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        self.history_rect.set(area);
        let content_width = area.width.saturating_sub(1);
        if content_width == 0 || area.height == 0 {
            return;
        }
        let viewport = u32::from(area.height);
        let mut turns = self.turns.borrow_mut();
        let mut in_flight = self.in_flight.borrow_mut();

        if self.width.get() != content_width {
            self.width.set(content_width);
            self.clear_selection();
            for slot in turns.iter_mut() {
                slot.cache = None;
            }
            if let Some(turn) = in_flight.as_mut() {
                turn.cache = None;
            }
        }

        let env = ChatEnv {
            lsp_diagnostics: &self.lsp_diagnostics,
            rev: self.env_rev,
        };
        let env_rev = self.env_rev;
        let streaming = self.streaming;
        let interrupted = self.interrupted;
        let turns_len = turns.len();

        let mut heights: Vec<u32> = turns
            .iter()
            .map(|slot| slot.height(content_width, env_rev))
            .collect();
        heights.push(
            in_flight
                .as_ref()
                .map_or(0, |turn| turn.height(content_width, env_rev)),
        );
        {
            let steered = self.steered.borrow();
            for slot in steered.iter() {
                heights.push(slot.height(content_width, env_rev));
            }
        }

        let sticky = self.scroll.borrow().sticky_bottom;
        let anchor = (!sticky).then(|| locate(&heights, self.scroll.borrow().offset));

        let offset = self.scroll.borrow().offset;
        let overscan = viewport;
        let lo = offset.saturating_sub(overscan);
        let hi = offset + viewport + overscan;

        {
            let stored = self.stored.as_ref();
            let toggled = &self.toggled;
            let mut y = 0u32;
            for (i, slot) in turns.iter_mut().enumerate() {
                let h = heights[i];
                if y + h > lo && y < hi {
                    let marker = interrupted && !streaming && i + 1 == turns_len;
                    if slot.blocks.is_none() {
                        let session = stored.expect("lazy turn without stored session");
                        slot.materialize(|| materialize_blocks(session, i), toggled, i, marker);
                    }
                    slot.ensure_cache(
                        i,
                        content_width,
                        &env,
                        env_rev,
                        TurnFlags {
                            in_flight: false,
                            interrupted_marker: marker,
                        },
                    );
                    heights[i] = slot.height(content_width, env_rev);
                }
                y += h;
            }
            if let Some(turn) = in_flight.as_mut() {
                let h = heights[turns_len];
                if y + h > lo && y < hi {
                    turn.ensure_cache(
                        turns_len,
                        content_width,
                        &env,
                        env_rev,
                        TurnFlags {
                            in_flight: true,
                            interrupted_marker: false,
                        },
                    );
                    heights[turns_len] = turn.height(content_width, env_rev);
                }
            }
            y += heights[turns_len];
            let mut steered = self.steered.borrow_mut();
            for (i, slot) in steered.iter_mut().enumerate() {
                let idx = turns_len + 1 + i;
                let h = heights[idx];
                if y + h > lo && y < hi {
                    slot.ensure_cache(
                        idx,
                        content_width,
                        &env,
                        env_rev,
                        TurnFlags {
                            in_flight: false,
                            interrupted_marker: false,
                        },
                    );
                    heights[idx] = slot.height(content_width, env_rev);
                }
                y += h;
            }
        }

        let total: u32 = heights.iter().sum();
        {
            let mut scroll = self.scroll.borrow_mut();
            if scroll.sticky_bottom {
                scroll.offset = total.saturating_sub(viewport);
            } else if let Some((turn_idx, intra)) = anchor {
                let start: u32 = heights.iter().take(turn_idx).sum();
                let h = heights.get(turn_idx).copied().unwrap_or(0);
                scroll.offset = start
                    .saturating_add(intra.min(h.saturating_sub(1)))
                    .min(total.saturating_sub(viewport));
            } else {
                scroll.offset = scroll.offset.min(total.saturating_sub(viewport));
            }
            scroll.sticky_bottom = scroll.offset >= total.saturating_sub(viewport);
        }
        let scroll_y = self.scroll.borrow().offset;

        {
            let buf = frame.buffer_mut();
            let stored = self.stored.as_ref();
            let toggled = &self.toggled;
            let mut y = 0u32;
            for (i, slot) in turns.iter_mut().enumerate() {
                let mut h = heights[i];
                if y + h > scroll_y && y < scroll_y + viewport {
                    if slot.blocks.is_none() {
                        let session = stored.expect("lazy turn without stored session");
                        slot.materialize(
                            || materialize_blocks(session, i),
                            toggled,
                            i,
                            interrupted && !streaming && i + 1 == turns_len,
                        );
                    }
                    slot.ensure_cache(
                        i,
                        content_width,
                        &env,
                        env_rev,
                        TurnFlags {
                            in_flight: false,
                            interrupted_marker: interrupted && !streaming && i + 1 == turns_len,
                        },
                    );
                    h = slot.height(content_width, env_rev);
                    heights[i] = h;
                    if let Some(cache) = slot.cache() {
                        paint_turn(cache, y, scroll_y, area, content_width, buf);
                    }
                }
                y += h;
            }
            let in_flight_h = if let Some(turn) = in_flight.as_mut() {
                let h = turn.height(content_width, env_rev);
                if y + h > scroll_y && y < scroll_y + viewport {
                    turn.ensure_cache(
                        turns_len,
                        content_width,
                        &env,
                        env_rev,
                        TurnFlags {
                            in_flight: true,
                            interrupted_marker: false,
                        },
                    );
                    if let Some(cache) = turn.cache() {
                        paint_turn(cache, y, scroll_y, area, content_width, buf);
                    }
                }
                Some(h)
            } else {
                None
            };
            y += in_flight_h.unwrap_or(0);
            let mut steered = self.steered.borrow_mut();
            for (i, slot) in steered.iter_mut().enumerate() {
                let idx = turns_len + 1 + i;
                let mut h = slot.height(content_width, env_rev);
                if y + h > scroll_y && y < scroll_y + viewport {
                    slot.ensure_cache(
                        idx,
                        content_width,
                        &env,
                        env_rev,
                        TurnFlags {
                            in_flight: false,
                            interrupted_marker: false,
                        },
                    );
                    h = slot.height(content_width, env_rev);
                    if let Some(cache) = slot.cache() {
                        paint_turn(cache, y, scroll_y, area, content_width, buf);
                    }
                }
                y += h;
            }

            if self.selection.borrow().is_some() {
                self.paint_selection_overlay(
                    &turns,
                    &in_flight,
                    &steered,
                    &heights,
                    turns_len,
                    scroll_y,
                    area,
                    content_width,
                    buf,
                );
            }
        }

        self.render_scrollbar(frame, area, scroll_y, total);
        self.evict(&mut turns, scroll_y, viewport);
    }

    /// Tint the selected content rows after the paint pass. Geometry-only:
    /// interior rows tint the full content width, boundary rows from the
    /// selection's column, snapped outward around wide glyphs.
    #[allow(clippy::too_many_arguments)]
    fn paint_selection_overlay(
        &self,
        turns: &[TurnData],
        in_flight: &Option<TurnData>,
        steered: &[TurnData],
        heights: &[u32],
        turns_len: usize,
        scroll_y: u32,
        clip: Rect,
        content_width: u16,
        buf: &mut Buffer,
    ) {
        let sel = *self.selection.borrow();
        let Some((start, end)) = sel.as_ref().and_then(SelRange::ordered) else {
            return;
        };
        let viewport = u32::from(clip.height);
        let mut y = 0u32;
        for (i, h) in heights.iter().enumerate() {
            let cache = if i < turns_len {
                turns.get(i).and_then(|slot| slot.cache.as_ref())
            } else if i == turns_len {
                in_flight.as_ref().and_then(|slot| slot.cache.as_ref())
            } else {
                steered
                    .get(i - turns_len - 1)
                    .and_then(|slot| slot.cache.as_ref())
            };
            let turn_idx = i;
            let h = *h;
            let lo = if turn_idx == start.turn {
                start.row.min(h.saturating_sub(1))
            } else {
                0
            };
            let hi = if turn_idx == end.turn {
                end.row.saturating_add(1).min(h)
            } else {
                h
            };
            if lo >= hi {
                y += h;
                continue;
            }
            if let Some(cache) = cache {
                for seg in &cache.segs {
                    let seg_lo = seg.start.max(lo).max(scroll_y.saturating_sub(y));
                    let seg_hi = (seg.start + seg.height)
                        .min(hi)
                        .min((scroll_y + viewport).saturating_sub(y));
                    if seg_lo >= seg_hi {
                        continue;
                    }
                    for row in seg_lo..seg_hi {
                        let global = y + row;
                        let (x0, x1) = sel_columns(
                            turn_idx,
                            row,
                            &start,
                            &end,
                            seg.segment.padding.0,
                            content_width,
                        );
                        if x0 >= x1 {
                            continue;
                        }
                        let screen_y =
                            clip.y + u16::try_from(global - scroll_y).unwrap_or(u16::MAX);
                        tint_row(buf, clip, content_width, screen_y, x0, x1);
                    }
                }
            }
            y += h;
        }
    }

    /// Borrow a turn slot by selection-space index: committed turns, then
    /// the in-flight turn, then the steered queue.
    fn with_turn<R>(&self, turn_idx: usize, f: impl FnOnce(&TurnData) -> R) -> Option<R> {
        let turns_len = self.turns.borrow().len();
        if turn_idx < turns_len {
            let turns = self.turns.borrow();
            return turns.get(turn_idx).map(f);
        }
        if turn_idx == turns_len {
            let in_flight = self.in_flight.borrow();
            return in_flight.as_ref().map(f);
        }
        let steered = self.steered.borrow();
        steered.get(turn_idx.checked_sub(turns_len + 1)?).map(f)
    }

    fn turn_count(&self) -> usize {
        self.turns.borrow().len()
            + usize::from(self.in_flight.borrow().is_some())
            + self.steered.borrow().len()
    }

    fn turn_height(&self, turn_idx: usize, width: u16) -> u32 {
        self.with_turn(turn_idx, |turn| turn.height(width, self.env_rev))
            .unwrap_or(0)
    }

    /// The content-space position under a terminal cell, `None` outside the
    /// history pane or past the last turn.
    fn sel_pos_at(&self, column: u16, row: u16) -> Option<SelPos> {
        if !self.in_history(column, row) {
            return None;
        }
        let rect = self.history_rect.get();
        let content_y = self.scroll.borrow().offset + u32::from(row - rect.y);
        let width = self.width.get();
        if width == 0 {
            return None;
        }
        let mut start = 0u32;
        for i in 0..self.turn_count() {
            let h = self.turn_height(i, width);
            if content_y < start + h {
                return Some(SelPos {
                    turn: i,
                    row: content_y - start,
                    col: column - rect.x,
                });
            }
            start += h;
        }
        None
    }

    fn handle_mouse(&mut self, kind: MouseKind, column: u16, row: u16) {
        match kind {
            MouseKind::Down => {
                self.clear_selection();
                self.dragging.set(false);
                let Some(pos) = self.sel_pos_at(column, row) else {
                    return;
                };
                if self.escalated_click(pos) {
                    return;
                }
                *self.selection.borrow_mut() = Some(SelRange {
                    anchor: pos,
                    head: pos,
                });
                self.dragging.set(true);
            }
            MouseKind::Drag => {
                if !self.dragging.get() {
                    return;
                }
                if let Some(pos) = self.sel_pos_at(column, row)
                    && let Some(sel) = self.selection.borrow_mut().as_mut()
                {
                    sel.head = pos;
                }
                self.drag_auto_scroll(row);
            }
            MouseKind::Up => {
                if !self.dragging.get() {
                    return;
                }
                self.dragging.set(false);
                let range = *self.selection.borrow();
                let Some(range) = range else { return };
                if range.drag_distance() < CLICK_SLOP {
                    self.clear_selection();
                    self.handle_click(column, row);
                } else if range.ordered().is_some() {
                    self.pending_copy.set(true);
                }
            }
        }
    }

    /// One row of auto-scroll when a drag reaches a viewport edge; the
    /// sticky-bottom state re-engages at the bottom through the next view.
    fn drag_auto_scroll(&self, row: u16) {
        let rect = self.history_rect.get();
        if rect.height == 0 {
            return;
        }
        let mut scroll = self.scroll.borrow_mut();
        if row <= rect.y {
            scroll.offset = scroll.offset.saturating_sub(1);
            scroll.sticky_bottom = false;
        } else if row + 1 >= rect.y + rect.height {
            scroll.offset = scroll.offset.saturating_add(1);
        }
    }

    fn clear_selection(&self) {
        *self.selection.borrow_mut() = None;
        self.pending_copy.set(false);
    }

    /// Escalate a same-cell click inside [`MULTI_CLICK_WINDOW`]: double
    /// selects the word, triple the whole logical row. The selection is
    /// final (dragging stays disarmed, copy is flagged); `true` when the
    /// click was consumed, so a failed resolve falls through to point start.
    fn escalated_click(&self, pos: SelPos) -> bool {
        let now = Instant::now();
        let count = match *self.last_click.borrow() {
            Some((last, at, count))
                if last == pos && now.duration_since(at) <= MULTI_CLICK_WINDOW =>
            {
                (count % 3) + 1
            }
            _ => 1,
        };
        *self.last_click.borrow_mut() = Some((pos, now, count));
        if count == 1 {
            return false;
        }
        let width = self.width.get();
        if width == 0 {
            return false;
        }
        let span = if count >= 3 {
            self.logical_row_range(pos, width)
        } else {
            self.word_range(pos)
        };
        let Some((anchor, head)) = span else {
            return false;
        };
        *self.selection.borrow_mut() = Some(SelRange { anchor, head });
        self.pending_copy.set(true);
        true
    }

    /// Resolve the visual row under `pos` for word/row picking.
    fn resolved_visual_row(&self, pos: SelPos) -> Option<ResolvedRow> {
        let width = self.width.get();
        self.with_turn(pos.turn, |slot| {
            let cache = slot.cache.as_ref()?;
            for seg in &cache.segs {
                if pos.row >= seg.start && pos.row < seg.start + seg.height {
                    return seg.segment.locate_row(pos.row - seg.start, width);
                }
            }
            None
        })
        .flatten()
    }

    /// The whitespace-delimited run of the visual row under `pos`.
    fn word_range(&self, pos: SelPos) -> Option<(SelPos, SelPos)> {
        let resolved = self.resolved_visual_row(pos)?;
        let text = visual_row_text(
            &resolved.line(),
            resolved.text_width,
            resolved.trim,
            resolved.wrap_index,
        );
        let text_col = usize::from(pos.col.saturating_sub(resolved.pad_x));
        let mut hit: Option<(usize, usize)> = None;
        let mut run_start: Option<usize> = None;
        let mut x = 0usize;
        for c in text.chars() {
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if c.is_whitespace() {
                if let Some(s) = run_start.take()
                    && text_col >= s
                    && text_col < x
                {
                    hit = Some((s, x));
                    break;
                }
            } else {
                run_start.get_or_insert(x);
            }
            x += w;
        }
        if hit.is_none()
            && let Some(s) = run_start
        {
            hit = Some((s, x));
        }
        let (start, end) = hit?;
        if start >= end {
            return None;
        }
        let anchor = SelPos {
            turn: pos.turn,
            row: pos.row,
            col: resolved.pad_x.saturating_add(start as u16),
        };
        let head = SelPos {
            turn: pos.turn,
            row: pos.row,
            col: resolved.pad_x.saturating_add(end as u16),
        };
        Some((anchor, head))
    }

    /// The full logical row (all wraps) under `pos`, spanning the row's
    /// content range so copy emits the clean logical text.
    fn logical_row_range(&self, pos: SelPos, width: u16) -> Option<(SelPos, SelPos)> {
        let resolved = self.resolved_visual_row(pos)?;
        let first = pos.row.saturating_sub(resolved.wrap_index);
        let last = pos.row.saturating_add(
            resolved
                .wrap_total
                .saturating_sub(1)
                .saturating_sub(resolved.wrap_index),
        );
        let anchor = SelPos {
            turn: pos.turn,
            row: first,
            col: 0,
        };
        let head = SelPos {
            turn: pos.turn,
            row: last,
            col: width,
        };
        Some((anchor, head))
    }

    /// Re-address the selection after the in-flight turn committed: its
    /// index now addresses the committed turn; steered turns shift by one.
    fn remap_after_commit(&self) {
        let in_flight_idx = self.turns.borrow().len().saturating_sub(1);
        if let Some(sel) = self.selection.borrow_mut().as_mut() {
            for pos in [&mut sel.anchor, &mut sel.head] {
                if pos.turn > in_flight_idx {
                    pos.turn += 1;
                }
            }
        }
    }

    /// Re-address the selection after steered[0] was dispatched: turns at
    /// its old index are gone, later steered turns shift down by one.
    fn remap_after_steered_removed(&self) {
        let removed = self.turns.borrow().len() + 1;
        if let Some(sel) = self.selection.borrow_mut().as_mut() {
            for pos in [&mut sel.anchor, &mut sel.head] {
                if pos.turn > removed {
                    pos.turn -= 1;
                }
            }
        }
        self.clear_selection_from(removed);
    }

    /// Drop the selection when it references a turn at or past `from`
    /// (removed steered turn, wiped in-flight, session replacement).
    fn clear_selection_from(&self, from: usize) {
        if self
            .selection
            .borrow()
            .as_ref()
            .is_some_and(|sel| sel.anchor.turn >= from || sel.head.turn >= from)
        {
            self.clear_selection();
        }
    }

    /// Materialize + render a turn on demand from `update` paths (toggle,
    /// click) outside the render loop.
    fn ensure_materialized(&mut self, turn_idx: usize) {
        let marker = {
            let turns = self.turns.borrow();
            match turns.get(turn_idx) {
                Some(slot) if slot.blocks.is_some() => return,
                _ => self.interrupted && !self.streaming && turn_idx + 1 == turns.len(),
            }
        };
        let mut turns = self.turns.borrow_mut();
        let Some(slot) = turns.get_mut(turn_idx) else {
            return;
        };
        if slot.blocks.is_some() {
            return;
        }
        let Some(session) = self.stored.as_ref() else {
            return;
        };
        slot.materialize(
            || materialize_blocks(session, turn_idx),
            &self.toggled,
            turn_idx,
            marker,
        );
    }

    /// Find the still-running block a finish belongs to and apply it. A
    /// finish carries the provider's call id, so batched calls of one tool
    /// (all `Running` at once) each land on their own block; the name+worker
    /// match is only a fallback for finishes without an id.
    fn with_running_tool(
        &mut self,
        name: &str,
        worker: &Option<String>,
        call_id: Option<&str>,
        apply: impl FnOnce(&mut Block) -> bool,
    ) -> bool {
        let is_match = |block: &Block| {
            if !block.tool_is_running() {
                return false;
            }
            let own = block.tool_call_id().unwrap_or_default();
            match call_id.filter(|id| !id.is_empty()) {
                Some(id) if !own.is_empty() => id == own,
                _ => block.tool_matches(name, worker),
            }
        };
        let mut in_flight = self.in_flight.borrow_mut();
        if let Some(turn) = in_flight.as_mut()
            && let Some(blocks) = turn.blocks.as_mut()
            && let Some(block) = blocks.iter_mut().rev().find(|block| is_match(block))
        {
            return apply(block);
        }
        // A finish that arrives after the turn was committed (a worker
        // straggler drained late) still lands on its block in the committed
        // turn; the rev bump forces the cached render to rebuild.
        let mut turns = self.turns.borrow_mut();
        for turn in turns.iter_mut().rev() {
            let Some(blocks) = turn.blocks.as_mut() else {
                continue;
            };
            let Some(block) = blocks.iter_mut().rev().find(|block| is_match(block)) else {
                continue;
            };
            let updated = apply(block);
            if updated {
                turn.rev += 1;
                turn.refresh_est(false);
            }
            return updated;
        }
        false
    }

    fn touch_in_flight(&mut self) {
        if let Some(turn) = self.in_flight.borrow_mut().as_mut() {
            turn.rev += 1;
            turn.refresh_est(false);
        }
    }

    fn commit_done(&mut self) {
        if let Some(mut turn) = self.in_flight.borrow_mut().take() {
            turn.finish_thinking();
            turn.rev += 1;
            turn.refresh_est(false);
            self.turns.borrow_mut().push(turn);
            self.interrupted = false;
            self.remap_after_commit();
        }
        self.streaming = false;
    }

    fn commit_interrupted(&mut self) {
        if let Some(mut turn) = self.in_flight.borrow_mut().take() {
            turn.finish_thinking();
            turn.kill_running_tools();
            let has_content = turn.blocks.as_ref().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.is_text() || matches!(block, Block::Reasoning(_) | Block::Tool(_))
                })
            });
            if has_content {
                self.interrupted = true;
                turn.rev += 1;
                turn.refresh_est(true);
                self.turns.borrow_mut().push(turn);
                self.remap_after_commit();
            } else {
                self.clear_selection_from(self.turns.borrow().len());
            }
        }
        self.streaming = false;
    }

    fn apply_session(&mut self, session: shuvarie_core::Session, reset_scroll: bool) {
        self.clear_selection();
        let interrupted = session.last_assistant_interrupted();
        let ests = build_turn_ests(&session, interrupted);
        let len = ests.len();
        *self.turns.borrow_mut() = session
            .messages
            .iter()
            .map(|message| message.role)
            .zip(ests)
            .map(|(role, est)| TurnData::lazy(role, est))
            .collect();
        *self.in_flight.borrow_mut() = None;
        self.stored = Some(session);
        self.stored_len = len;
        self.streaming = false;
        self.interrupted = interrupted;
        self.toggled.clear();
        if reset_scroll {
            let mut scroll = self.scroll.borrow_mut();
            scroll.offset = 0;
            scroll.sticky_bottom = false;
        }
    }

    /// Text of the active chat selection, copied on demand; `None` when no
    /// selection is live. Fully covered logical rows copy clean logical
    /// text (gutterless code, decorated tool rows); boundary rows copy the
    /// visual fragment the pixels show.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = (*self.selection.borrow()).and_then(|r| r.ordered())?;
        let width = self.width.get();
        if width == 0 {
            return None;
        }
        let last = self.turn_count().saturating_sub(1);
        if start.turn > last {
            return None;
        }
        let end_turn = end.turn.min(last);

        let mut out: Vec<Option<String>> = Vec::new();
        let mut group: Option<CopyGroup> = None;
        for turn in start.turn..=end_turn {
            let turn_h = self.turn_height(turn, width);
            if turn_h == 0 {
                continue;
            }
            let lo = if turn == start.turn {
                start.row.min(turn_h - 1)
            } else {
                0
            };
            let hi = if turn == end_turn {
                end.row.min(turn_h - 1)
            } else {
                turn_h - 1
            };
            if lo > hi {
                continue;
            }
            for row in lo..=hi {
                let hit = self.with_turn(turn, |slot| {
                    let cache = slot.cache.as_ref()?;
                    for (seg_idx, seg) in cache.segs.iter().enumerate() {
                        if row >= seg.start && row < seg.start + seg.height {
                            return seg
                                .segment
                                .locate_row(row - seg.start, width)
                                .map(|resolved| (seg_idx, resolved));
                        }
                    }
                    None
                });
                let from = (turn == start.turn && row == start.row).then_some(start.col);
                let to = (turn == end_turn && row == end.row).then_some(end.col);
                let hit = hit.flatten();
                let cols = hit.as_ref().map(|(_, r)| normalize_cols(r, from, to));
                match hit {
                    Some((seg_idx, resolved)) => {
                        let key = (turn, seg_idx, resolved.source_row);
                        let bounds = cols.expect("resolved row carries normalized bounds");
                        match &mut group {
                            Some(g) if g.key == key => g.wraps.push((resolved.wrap_index, bounds)),
                            _ => {
                                if let Some(done) = group.take() {
                                    done.flush(&mut out);
                                }
                                group = Some(CopyGroup {
                                    key,
                                    wraps: vec![(resolved.wrap_index, bounds)],
                                    resolved,
                                });
                            }
                        }
                    }
                    None => {
                        if let Some(done) = group.take() {
                            done.flush(&mut out);
                        }
                        out.push(None);
                    }
                }
            }
        }
        if let Some(done) = group.take() {
            done.flush(&mut out);
        }

        let mut rows: Vec<String> = Vec::new();
        let mut blanks = 0usize;
        for entry in out {
            match entry {
                Some(text) => {
                    for _ in 0..blanks {
                        rows.push(String::new());
                    }
                    blanks = 0;
                    rows.push(text);
                }
                None => blanks += 1,
            }
        }
        if rows.is_empty() {
            None
        } else {
            Some(rows.join("\n"))
        }
    }

    /// Copy-on-select hook: the selection's text once, right after a drag
    /// finalized it, then cleared.
    pub fn take_pending_copy(&self) -> Option<String> {
        if !self.pending_copy.replace(false) {
            return None;
        }
        self.selected_text()
    }

    fn in_history(&self, column: u16, row: u16) -> bool {
        let rect = self.history_rect.get();
        rect.width > 0
            && rect.height > 0
            && column >= rect.x
            && column < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    fn handle_click(&mut self, column: u16, row: u16) {
        if !self.in_history(column, row) {
            return;
        }
        let rect = self.history_rect.get();
        let content_y = self.scroll.borrow().offset + u32::from(row - rect.y);
        let width = self.width.get();
        let env_rev = self.env_rev;
        let mut start = 0u32;
        let mut target = None;
        {
            let turns = self.turns.borrow();
            for slot in turns.iter() {
                let h = slot.height(width, env_rev);
                if content_y < start + h {
                    if let Some(cache) = slot.cache() {
                        let local = content_y - start;
                        target = cache
                            .hits
                            .iter()
                            .find(|region| local >= region.start && local < region.end)
                            .map(|region| region.addr.clone());
                    }
                    break;
                }
                start += h;
            }
        }
        if target.is_none() {
            let in_flight = self.in_flight.borrow();
            if let Some(turn) = in_flight.as_ref() {
                let h = turn.height(width, env_rev);
                if content_y < start + h
                    && let Some(cache) = turn.cache()
                {
                    let local = content_y - start;
                    target = cache
                        .hits
                        .iter()
                        .find(|region| local >= region.start && local < region.end)
                        .map(|region| region.addr.clone());
                }
            }
        }
        if target.is_none() {
            let in_flight_h = self
                .in_flight
                .borrow()
                .as_ref()
                .map_or(0, |turn| turn.height(width, env_rev));
            start += in_flight_h;
            let steered = self.steered.borrow();
            for turn in steered.iter() {
                let h = turn.height(width, env_rev);
                if content_y < start + h {
                    if let Some(cache) = turn.cache() {
                        let local = content_y - start;
                        target = cache
                            .hits
                            .iter()
                            .find(|region| local >= region.start && local < region.end)
                            .map(|region| region.addr.clone());
                    }
                    break;
                }
                start += h;
            }
        }
        if let Some(addr) = target {
            self.toggle_block(addr);
        }
    }

    fn toggle_block(&mut self, addr: BlockAddr) {
        self.clear_selection();
        let turns_len = self.turns.borrow().len();
        if addr.turn < turns_len {
            self.ensure_materialized(addr.turn);
            let mut turns = self.turns.borrow_mut();
            let Some(slot) = turns.get_mut(addr.turn) else {
                return;
            };
            let changed = slot
                .blocks
                .as_mut()
                .and_then(|blocks| blocks.get_mut(addr.block))
                .is_some_and(|block| block.update(BlockMessage::Toggle));
            if changed {
                let expanded = slot
                    .blocks
                    .as_ref()
                    .and_then(|blocks| blocks.get(addr.block))
                    .is_some_and(Block::is_expanded);
                if expanded {
                    self.toggled.insert((addr.turn, addr.block));
                } else {
                    self.toggled.remove(&(addr.turn, addr.block));
                }
                slot.rev += 1;
                slot.refresh_est(self.interrupted && !self.streaming && addr.turn + 1 == turns_len);
            }
        } else if addr.turn == turns_len {
            let mut in_flight = self.in_flight.borrow_mut();
            let Some(turn) = in_flight.as_mut() else {
                return;
            };
            let changed = turn
                .blocks
                .as_mut()
                .and_then(|blocks| blocks.get_mut(addr.block))
                .is_some_and(|block| block.update(BlockMessage::Toggle));
            if changed {
                let expanded = turn
                    .blocks
                    .as_ref()
                    .and_then(|blocks| blocks.get(addr.block))
                    .is_some_and(Block::is_expanded);
                if expanded {
                    self.toggled.insert((addr.turn, addr.block));
                } else {
                    self.toggled.remove(&(addr.turn, addr.block));
                }
                turn.rev += 1;
                turn.refresh_est(false);
            }
        }
    }

    fn toggle_last_tool(&mut self) {
        let turns_len = self.turns.borrow().len();
        let in_flight_tool = {
            let in_flight = self.in_flight.borrow();
            in_flight.as_ref().and_then(|turn| {
                turn.blocks
                    .as_ref()
                    .and_then(|blocks| blocks.iter().rposition(Block::is_tool))
            })
        };
        if let Some(block) = in_flight_tool {
            let addr = BlockAddr {
                turn: turns_len,
                block,
            };
            self.toggle_block(addr);
            return;
        }
        for turn_idx in (0..turns_len).rev() {
            {
                let turns = self.turns.borrow();
                let slot = &turns[turn_idx];
                if slot.blocks.is_none() && slot.est.tool_count == 0 {
                    continue;
                }
            }
            self.ensure_materialized(turn_idx);
            let found = self.turns.borrow()[turn_idx]
                .blocks
                .as_ref()
                .and_then(|blocks| blocks.iter().rposition(Block::is_tool));
            if let Some(block) = found {
                self.toggle_block(BlockAddr {
                    turn: turn_idx,
                    block,
                });
                return;
            }
        }
    }

    /// Evict rendered state for turns far outside the viewport: lazy-backed
    /// turns drop their block models too (re-materializable from the stored
    /// session); live turns keep blocks but drop the render cache.
    fn evict(&self, turns: &mut [TurnData], scroll_y: u32, viewport: u32) {
        let margin = 3 * viewport.max(1);
        let lo = scroll_y.saturating_sub(margin);
        let hi = scroll_y + viewport + margin;
        let width = self.width.get();
        let env_rev = self.env_rev;
        let mut y = 0u32;
        for (i, slot) in turns.iter_mut().enumerate() {
            let h = slot.height(width, env_rev);
            if y + h <= lo || y >= hi {
                if i < self.stored_len {
                    slot.blocks = None;
                }
                slot.cache = None;
            }
            y += h;
        }
    }

    fn render_scrollbar(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        scroll_y: u32,
        content_height: u32,
    ) {
        let track_len = area.height as usize;
        if track_len < 2 || content_height <= u32::from(area.height) {
            return;
        }
        let content_len = content_height as usize;
        let viewport_len = area.height as usize;
        let offset = scroll_y as usize;
        let max_offset = content_len - viewport_len;
        let thumb_len = (track_len * viewport_len / content_len).clamp(1, track_len);
        let max_start = track_len - thumb_len;
        let thumb_start = (max_start * offset)
            .checked_div(max_offset)
            .unwrap_or(0)
            .min(max_start);
        let bar_x = area.right().saturating_sub(1);
        let buf = frame.buffer_mut();
        for row in area.top()..area.bottom() {
            let y = row as usize;
            let (symbol, style) = if (thumb_start..thumb_start + thumb_len).contains(&y) {
                ("█", theme::ACCENT)
            } else {
                (" ", theme::TEXT_MUTED)
            };
            let cell = buf.cell_mut((bar_x, row)).expect("bar_x in bounds");
            cell.set_symbol(symbol);
            cell.set_style(Style::new().fg(style));
        }
    }
}

/// Rebuild the turns of a stored session: user/system messages become their
/// single blocks, assistant messages assemble reasoning, summary marker, tool
/// blocks (in record order, before the text — matching reload layout), and
/// the text chunk.
fn materialize_blocks(session: &shuvarie_core::Session, idx: usize) -> Vec<Block> {
    let message = &session.messages[idx];
    let mut blocks = Vec::new();
    match message.role {
        Role::User => blocks.push(Block::User(UserPrompt::new(message.content.clone()))),
        Role::System => blocks.push(Block::System(SystemText::new(message.content.clone()))),
        Role::Assistant => {
            let segments: &[ReasoningSegment] = session
                .reasoning
                .get(&(idx as u64))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let mut seg_i = 0usize;
            let drain_reasoning =
                |blocks: &mut Vec<Block>, seg_i: &mut usize, tools_done: usize| {
                    while let Some(seg) = segments.get(*seg_i)
                        && (seg.after_tool as usize) <= tools_done
                    {
                        blocks.push(Block::Reasoning(ReasoningBlock::finished(
                            seg.text.clone(),
                            seg.duration_ms,
                        )));
                        *seg_i += 1;
                    }
                };
            drain_reasoning(&mut blocks, &mut seg_i, 0);
            if session.summary_seq.is_some_and(|seq| seq as usize == idx) {
                blocks.push(Block::Summary);
            }
            for (count, record) in session
                .tool_records
                .iter()
                .filter(|record| record.message_seq as usize == idx)
                .enumerate()
            {
                blocks.push(Block::Tool(Box::new(ToolBlock::from_record(record))));
                drain_reasoning(&mut blocks, &mut seg_i, count + 1);
            }
            drain_reasoning(&mut blocks, &mut seg_i, usize::MAX);
            if !message.content.is_empty() {
                blocks.push(Block::Text(TextBlock::new(message.content.clone())));
            }
        }
    }
    blocks
}

/// Precompute the lazy-turn height estimates of a stored session in one pass:
/// tool records are grouped per message so the walk stays linear.
fn build_turn_ests(session: &shuvarie_core::Session, interrupted: bool) -> Vec<TurnEst> {
    let mut by_msg: BTreeMap<u64, Vec<&ToolRecord>> = BTreeMap::new();
    for record in &session.tool_records {
        by_msg.entry(record.message_seq).or_default().push(record);
    }
    let last = session.messages.len().saturating_sub(1);
    session
        .messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let tools: Vec<&ToolRecord> = by_msg.get(&(idx as u64)).cloned().unwrap_or_default();
            let reasoning_count = session.reasoning.get(&(idx as u64)).map_or(0, Vec::len);
            let summary = session.summary_seq.is_some_and(|seq| seq as usize == idx);
            TurnEst::from_session_parts(
                message.role,
                &message.content,
                &tools,
                reasoning_count,
                summary,
                interrupted && idx == last && message.role == Role::Assistant,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use shuvarie_core::tool_record::ToolRecord;

    use crate::tui::session::blocks::ReasoningMessage;
    use crate::tui::session::virtualizer::render_turn_cache;

    fn header_text(block: &Block) -> String {
        let Block::Reasoning(reasoning) = block else {
            panic!("not a reasoning block");
        };
        reasoning
            .view(80)
            .first()
            .and_then(|segment| segment.flattened().into_iter().next())
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>()
            })
            .unwrap_or_default()
    }

    fn block_tags(blocks: &[Block]) -> Vec<&'static str> {
        blocks
            .iter()
            .map(|block| match block {
                Block::Reasoning(_) => "R",
                Block::Tool(_) => "T",
                Block::Text(_) => "X",
                Block::User(_) => "U",
                _ => "?",
            })
            .collect()
    }

    fn tool_record(seq: u64) -> ToolRecord {
        ToolRecord {
            name: "read_file".to_string(),
            args_json: "{}".to_string(),
            output: String::new(),
            stderr: String::new(),
            ok: true,
            killed: false,
            worker: None,
            message_id: 1,
            message_seq: seq,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 0,
        }
    }

    fn draw(chat: &Chat, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| chat.view(frame, Rect::new(0, 0, width, height)))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_turn_lines(chat: &Chat, turn_idx: Option<usize>, width: u16) -> Option<String> {
        let diags = BTreeMap::new();
        let env = ChatEnv {
            lsp_diagnostics: &diags,
            rev: 0,
        };
        let cache = match turn_idx {
            Some(idx) => {
                let turns = chat.turns.borrow();
                let slot = turns.get(idx)?;
                render_turn_cache(
                    slot,
                    idx,
                    TurnFlags {
                        in_flight: false,
                        interrupted_marker: false,
                    },
                    width,
                    &env,
                    0,
                )?
            }
            None => {
                let turn = chat.in_flight.borrow();
                let turn = turn.as_ref()?;
                render_turn_cache(
                    turn,
                    chat.turns.borrow().len(),
                    TurnFlags {
                        in_flight: true,
                        interrupted_marker: false,
                    },
                    width,
                    &env,
                    0,
                )?
            }
        };
        Some(
            cache
                .segs
                .iter()
                .flat_map(|seg| {
                    seg.segment
                        .flattened()
                        .iter()
                        .map(|line| {
                            line.spans
                                .iter()
                                .map(|span| span.content.clone())
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    #[test]
    fn working_placeholder_hidden_while_thinking() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::ReasoningReceived {
            content: "hmm".into(),
        });
        let thinking = render_turn_lines(&chat, None, 80).unwrap();
        assert!(thinking.contains("Thinking..."));
        assert!(
            !thinking.contains("(Working...)"),
            "no placeholder while thinking"
        );

        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        });
        let tool_only = render_turn_lines(&chat, None, 80).unwrap();
        assert!(tool_only.contains("(Working...)"));
    }

    #[test]
    fn thinking_header_shows_spinner_then_thought() {
        let block = ReasoningBlock::new("hmm");
        let thinking = header_text(&Block::Reasoning(block));
        assert!(thinking.contains("Thinking..."), "header: {thinking}");
        assert!(!thinking.contains("Thought"));

        let mut block = ReasoningBlock::new("hmm");
        assert!(block.update(ReasoningMessage::Finish));
        let done = header_text(&Block::Reasoning(block));
        assert!(done.contains("Thought"), "header: {done}");

        let done = header_text(&Block::Reasoning(ReasoningBlock::finished("hmm", 0)));
        assert!(done.contains("Thought"), "reloaded header: {done}");

        let done = header_text(&Block::Reasoning(ReasoningBlock::finished("hmm", 10_300)));
        assert!(done.contains("Thought 10.3s"), "reloaded header: {done}");
    }

    #[test]
    fn text_and_tools_finish_thinking() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::ReasoningReceived {
            content: "first".into(),
        });
        chat.update(ChatMessage::TokenReceived {
            content: "answer".into(),
        });
        let inspect = |chat: &Chat| {
            let in_flight = chat.in_flight.borrow();
            let blocks = in_flight.as_ref().unwrap().blocks.as_deref().unwrap();
            let headers: Vec<String> = blocks
                .iter()
                .filter(|block| matches!(block, Block::Reasoning(_)))
                .map(header_text)
                .collect();
            (block_tags(blocks), headers)
        };
        let (tags, headers) = inspect(&chat);
        assert_eq!(tags, vec!["R", "X"]);
        assert!(headers[0].contains("Thought"));

        chat.update(ChatMessage::ReasoningReceived {
            content: "more".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ReasoningReceived {
            content: "again".into(),
        });
        let (tags, headers) = inspect(&chat);
        assert_eq!(tags, vec!["R", "X", "R", "T", "R"]);
        assert!(headers[0].contains("Thought"));
        assert!(headers[1].contains("Thought"));
        assert!(headers[2].contains("Thinking..."));
    }

    #[test]
    #[ignore]
    fn render_perf_probe() {
        let unit = "## Section\n\nSome prose explaining the next steps in detail.\n\n```rust\nfn main() {\n    let x = compute(42);\n    println!(\"{x}\");\n}\n```\n\n";
        for &text_units in &[10usize, 40, 160] {
            let mut chat = Chat::new();
            chat.update(ChatMessage::BeginUserTurn {
                content: "go".into(),
            });
            for i in 0..40 {
                chat.update(ChatMessage::ToolStarted {
                    name: "read_file".into(),
                    args: serde_json::json!({"path": format!("src/a/long/path/file_{i}.rs")}),
                    worker: Some("explore".into()),
                    call_id: None,
                });
                chat.update(ChatMessage::TokenReceived {
                    content: unit.repeat(2),
                });
                chat.update(ChatMessage::ToolFinished {
                    name: "read_file".into(),
                    ok: true,
                    output: "ok line\n".repeat(400),
                    worker: Some("explore".into()),
                    file_change: None,
                    streams: None,
                    duration_ms: 120,
                    call_id: None,
                });
            }
            chat.update(ChatMessage::TokenReceived {
                content: unit.repeat(text_units),
            });
            let t0 = std::time::Instant::now();
            draw(&chat, 100, 40);
            let cold = t0.elapsed();
            let mut worst = std::time::Duration::ZERO;
            for _ in 0..5 {
                let t1 = std::time::Instant::now();
                chat.update(ChatMessage::SpinnerUpdate);
                draw(&chat, 100, 40);
                worst = worst.max(t1.elapsed());
            }
            eprintln!(
                "text_units={text_units} cold={cold:?} spinner_rebuild_worst={warm:?}",
                warm = worst
            );
        }
    }

    #[test]
    fn commit_done_keeps_running_worker_tool_block() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::WorkerStarted {
            name: "explore".into(),
            args: serde_json::json!("find it"),
            call_id: None,
        });
        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({"path": "x"}),
            worker: Some("explore".into()),
            call_id: None,
        });
        chat.update(ChatMessage::StreamDone);
        assert!(chat.in_flight.borrow().is_none());
        let turns = chat.turns.borrow();
        let blocks = turns.last().unwrap().blocks.as_deref().unwrap();
        let running: Vec<_> = blocks
            .iter()
            .filter(|block| {
                let Block::Tool(tool) = block else {
                    return false;
                };
                tool.is_running()
            })
            .collect();
        assert_eq!(
            running.len(),
            2,
            "running blocks leaked into a committed turn"
        );
    }

    #[test]
    fn late_tool_started_reopens_in_flight_turn() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::StreamDone);
        assert!(chat.in_flight.borrow().is_none());
        chat.update(ChatMessage::ToolStarted {
            name: "grep".into(),
            args: serde_json::json!({}),
            worker: Some("explore".into()),
            call_id: None,
        });
        assert!(
            chat.in_flight.borrow().is_some(),
            "a late ToolStarted re-created an in-flight turn after commit"
        );
    }

    #[test]
    fn late_tool_finished_finishes_committed_block() {
        // A worker straggler drained after `StreamDone`: the running block
        // lives in the committed turn and must still be finished there.
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::WorkerStarted {
            name: "explore".into(),
            args: serde_json::json!("find it"),
            call_id: None,
        });
        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({"path": "x"}),
            worker: Some("explore".into()),
            call_id: None,
        });
        chat.update(ChatMessage::StreamDone);
        assert!(chat.has_running_tool_blocks());

        let turns = chat.turns.borrow();
        let rev_before = turns.last().unwrap().rev;
        drop(turns);

        chat.update(ChatMessage::ToolFinished {
            name: "read_file".into(),
            ok: true,
            output: "contents".into(),
            worker: Some("explore".into()),
            file_change: None,
            streams: None,
            duration_ms: 120,
            call_id: None,
        });
        assert!(
            chat.has_running_tool_blocks(),
            "the worker call itself is still running"
        );
        let turns = chat.turns.borrow();
        let turn = turns.last().unwrap();
        assert!(
            turn.rev > rev_before,
            "committed turn cache must rebuild after a late finish"
        );
        let blocks = turn.blocks.as_deref().unwrap();
        let Block::Tool(tool) = blocks.last().unwrap() else {
            panic!("expected tool block")
        };
        assert!(!tool.is_running());
    }

    #[test]
    fn running_committed_block_keeps_wake_armed() {
        // The render loop's spinner wake comes from `has_running_tool_blocks`
        // when `busy` is false, so a committed straggler keeps animating.
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "grep".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        });
        assert!(chat.has_running_tool_blocks());
        chat.update(ChatMessage::StreamDone);
        assert!(chat.has_running_tool_blocks(), "straggler stays armed");
        chat.update(ChatMessage::ToolFinished {
            name: "grep".into(),
            ok: true,
            output: "out".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 5,
            call_id: None,
        });
        assert!(!chat.has_running_tool_blocks());
        chat.update(ChatMessage::ToolFinished {
            name: "grep".into(),
            ok: true,
            output: "out".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 5,
            call_id: None,
        });
        assert!(!chat.has_running_tool_blocks(), "stale finish is a no-op");
    }

    #[test]
    fn batched_same_tool_finishes_land_on_their_own_blocks() {
        // A model can batch several calls of one tool in a single response:
        // rig streams every call's start before any result, so all blocks are
        // running at once. Each finish must land on the block of its own
        // call id — matching by name alone reverses the outputs onto the
        // sibling blocks (the first block would show the newest list).
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "todo".into(),
            args: serde_json::json!({ "op": "add", "text": "first" }),
            worker: None,
            call_id: Some("call-1".into()),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "todo".into(),
            args: serde_json::json!({ "op": "add", "text": "second" }),
            worker: None,
            call_id: Some("call-2".into()),
        });
        chat.update(ChatMessage::ToolFinished {
            name: "todo".into(),
            ok: true,
            output: "Todos (0/1 done)\n  #1 [ ] first".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 1,
            call_id: Some("call-1".into()),
        });
        chat.update(ChatMessage::ToolFinished {
            name: "todo".into(),
            ok: true,
            output: "Todos (0/2 done)\n  #1 [ ] first\n  #2 [ ] second".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 1,
            call_id: Some("call-2".into()),
        });
        let text = render_turn_lines(&chat, None, 80).unwrap();
        let first_header = text.find("todo + \"first\"").expect("first header");
        let second_header = text.find("todo + \"second\"").expect("second header");
        assert!(
            first_header < second_header,
            "blocks stay in call order: {text}"
        );
        let one_done = text.find("0/1 done").expect("first list count");
        let two_done = text.find("0/2 done").expect("second list count");
        assert!(
            one_done < two_done,
            "each block shows its own call's snapshot: {text}"
        );
        assert!(
            one_done < second_header,
            "the first block's body precedes the second block: {text}"
        );
    }

    #[test]
    fn finish_without_call_id_falls_back_to_name_and_worker() {
        // Shell-output paths (and legacy flows) have no call id; they match
        // by name + worker as before.
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "run_shell".into(),
            args: serde_json::json!({"command": "ls"}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolFinished {
            name: "run_shell".into(),
            ok: true,
            output: "exit 0\nout".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 3,
            call_id: None,
        });
        let text = render_turn_lines(&chat, None, 80).unwrap();
        assert!(text.contains("out"), "finish landed on the block: {text}");
        assert!(
            text.contains("Took"),
            "finished blocks show the took meta row: {text}"
        );
    }

    #[test]
    fn commit_finishes_thinking() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::ReasoningReceived {
            content: "only thoughts".into(),
        });
        chat.update(ChatMessage::StreamDone);
        let turns = chat.turns.borrow();
        let blocks = turns.last().unwrap().blocks.as_deref().unwrap();
        assert_eq!(block_tags(blocks), vec!["R"]);
        assert!(header_text(&blocks[0]).contains("Thought"));
    }

    #[test]
    fn reload_interleaves_reasoning_between_tool_records() {
        let mut session = shuvarie_core::Session::new();
        session.push_user("do it");
        session.push_assistant("done");
        session.reasoning.insert(
            1,
            vec![
                ReasoningSegment {
                    after_tool: 0,
                    text: "start".to_string(),
                    duration_ms: 0,
                },
                ReasoningSegment {
                    after_tool: 2,
                    text: "after two tools".to_string(),
                    duration_ms: 0,
                },
                ReasoningSegment {
                    after_tool: 9,
                    text: "beyond records".to_string(),
                    duration_ms: 0,
                },
            ],
        );
        session.tool_records = vec![tool_record(1), tool_record(1)];

        let blocks = materialize_blocks(&session, 1);
        assert_eq!(block_tags(&blocks), vec!["R", "T", "T", "R", "R", "X"]);
        assert!(header_text(&blocks[0]).contains("Thought"));
    }

    #[test]
    fn diagnostics_only_invalidate_tool_turns() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "check".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "edit_file".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolFinished {
            name: "edit_file".into(),
            ok: true,
            output: String::new(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
            call_id: None,
        });
        chat.update(ChatMessage::StreamDone);
        chat.update(ChatMessage::BeginUserTurn {
            content: "plain".into(),
        });
        chat.update(ChatMessage::TokenReceived {
            content: "reply".into(),
        });
        chat.update(ChatMessage::StreamDone);
        draw(&chat, 80, 20);

        chat.update(ChatMessage::LspDiagnostics {
            path: "src/lib.rs".into(),
            diagnostics: vec![],
        });

        let turns = chat.turns.borrow();
        let tool_turn = &turns[1];
        let text_turn = &turns[3];
        assert!(tool_turn.env_relevant);
        assert!(!text_turn.env_relevant);
        assert!(
            !tool_turn.cache.as_ref().unwrap().matches(
                79,
                tool_turn.rev,
                tool_turn.env_relevant,
                chat.env_rev
            ),
            "tool turn cache invalidated by env bump"
        );
        assert!(
            text_turn.cache.as_ref().unwrap().matches(
                79,
                text_turn.rev,
                text_turn.env_relevant,
                chat.env_rev
            ),
            "text turn cache survives env bump"
        );
    }

    #[test]
    fn in_flight_rerender_is_stable() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "make it so".into(),
        });
        chat.update(ChatMessage::TokenReceived {
            content: "hello".into(),
        });
        let first = render_turn_lines(&chat, None, 80).unwrap();

        chat.update(ChatMessage::TokenReceived {
            content: " world".into(),
        });
        let second = render_turn_lines(&chat, None, 80).unwrap();
        assert_ne!(first, second);

        let again = render_turn_lines(&chat, None, 80).unwrap();
        assert_eq!(second, again, "same state renders identically");
    }

    /// A session whose turns render much taller than their O(1) estimates:
    /// single-line tool outputs that wrap over many rows and long reasoning
    /// paragraphs. This is the shape that made estimate↔exact height flapping
    /// visible as scroll jitter.
    fn est_hostile_session(turns: usize) -> shuvarie_core::Session {
        let mut session = shuvarie_core::Session::new();
        for i in 0..turns {
            session.push_user(format!("do task {i}"));
            session.push_assistant(format!("done task {i}"));
            let idx = session.messages.len() as u64 - 1;
            session.reasoning.insert(
                idx,
                vec![ReasoningSegment {
                    after_tool: 0,
                    text: format!("thinking about task {i} ") + &"y".repeat(600),
                    duration_ms: 0,
                }],
            );
            session.tool_records.push(ToolRecord {
                name: "run_shell".to_string(),
                args_json: format!("{{\"command\":\"{}\"}}", "c".repeat(290)),
                output: "x".repeat(1200),
                stderr: String::new(),
                ok: true,
                killed: false,
                worker: None,
                message_id: i as u64,
                message_seq: idx,
                file_change: None,
                original_content: None,
                new_content: None,
                duration_ms: 0,
            });
        }
        session
    }

    #[test]
    fn turn_height_stays_measured_when_cache_invalidated() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::Load {
            session: est_hostile_session(6),
        });
        draw(&chat, 80, 20);
        let measured = chat.turns.borrow()[1].height(79, chat.env_rev);
        let est = chat.turns.borrow()[1].est.height(79);
        assert_ne!(
            measured, est,
            "session must be estimate-hostile for this test"
        );

        chat.update(ChatMessage::LspDiagnostics {
            path: "src/main.rs".into(),
            diagnostics: vec![],
        });
        assert_eq!(
            chat.turns.borrow()[1].height(79, chat.env_rev),
            measured,
            "env bump must not flip the layout height back to the estimate"
        );

        chat.turns.borrow_mut()[1].rev += 1;
        assert_eq!(
            chat.turns.borrow()[1].height(79, chat.env_rev),
            measured,
            "rev bump must not flip the layout height back to the estimate"
        );
    }

    #[test]
    fn scroll_up_sticks_and_anchor_holds_while_streaming() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::Load {
            session: est_hostile_session(30),
        });
        for _ in 0..3000 {
            chat.update(ChatMessage::ScrollDown);
        }
        draw(&chat, 80, 20);
        for _ in 0..10 {
            chat.update(ChatMessage::ScrollUp);
        }
        draw(&chat, 80, 20);
        assert!(
            !chat.scroll.borrow().sticky_bottom,
            "scroll-up disengages sticky"
        );
        let held = chat.scroll.borrow().offset;

        for i in 0..30 {
            chat.update(ChatMessage::TokenReceived {
                content: format!("word{i} "),
            });
            chat.update(ChatMessage::SpinnerUpdate);
            if i % 3 == 0 {
                chat.update(ChatMessage::LspDiagnostics {
                    path: "src/main.rs".into(),
                    diagnostics: vec![],
                });
            }
            draw(&chat, 80, 20);
            let scroll = chat.scroll.borrow();
            assert_eq!(
                scroll.offset, held,
                "frame {i}: anchor must hold the viewport while streaming"
            );
            assert!(
                !scroll.sticky_bottom,
                "frame {i}: streaming must not re-engage sticky"
            );
        }

        for _ in 0..3000 {
            chat.update(ChatMessage::ScrollDown);
        }
        draw(&chat, 80, 20);
        assert!(
            chat.scroll.borrow().sticky_bottom,
            "scrolling back to the bottom re-engages sticky"
        );
    }

    fn session_with_user_turns(count: usize) -> shuvarie_core::Session {
        let mut session = shuvarie_core::Session::new();
        for i in 0..count {
            session.push_user(format!("prompt number {i}"));
            session.push_assistant(format!("reply number {i}"));
        }
        session
    }

    fn session_with_tool_turns(count: usize) -> shuvarie_core::Session {
        let mut session = shuvarie_core::Session::new();
        for i in 0..count {
            session.push_user(format!("do {i}"));
            session.push_assistant(format!("done {i}"));
            session.tool_records.push(ToolRecord {
                name: "read_file".to_string(),
                args_json: format!("{{\"path\":\"f{i}.rs\"}}"),
                output: (0..3)
                    .map(|l| format!("out {i}-{l}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                stderr: String::new(),
                ok: true,
                killed: false,
                worker: None,
                message_id: i as u64,
                message_seq: (2 * i + 1) as u64,
                file_change: None,
                original_content: None,
                new_content: None,
                duration_ms: 0,
            });
        }
        session
    }

    #[test]
    fn lazy_load_materializes_window_only() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::Load {
            session: session_with_user_turns(60),
        });
        draw(&chat, 80, 20);
        {
            let turns = chat.turns.borrow();
            let materialized = turns.iter().filter(|t| t.blocks.is_some()).count();
            assert!(
                materialized > 0 && materialized < 60,
                "materialized {materialized}"
            );
            assert!(turns.last().unwrap().blocks.is_none());
            assert!(turns.first().unwrap().blocks.is_some());
        }

        for _ in 0..6000 {
            chat.update(ChatMessage::ScrollDown);
        }
        draw(&chat, 80, 20);
        let turns = chat.turns.borrow();
        assert!(
            turns.first().unwrap().blocks.is_none(),
            "top turns evicted after scrolling away"
        );
        assert!(turns.last().unwrap().blocks.is_some());
    }

    #[test]
    fn sticky_bottom_follows_tokens_until_scroll_up() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "hi".into(),
        });
        for i in 0..40 {
            chat.update(ChatMessage::TokenReceived {
                content: format!("line {i}\n"),
            });
        }
        draw(&chat, 80, 10);
        assert!(chat.scroll.borrow().sticky_bottom);
        let bottom = chat.scroll.borrow().offset;
        assert!(bottom > 0, "streaming content exceeds the viewport");

        chat.update(ChatMessage::ScrollUp);
        assert!(!chat.scroll.borrow().sticky_bottom);
        chat.update(ChatMessage::TokenReceived {
            content: "more text\n".into(),
        });
        draw(&chat, 80, 10);
        assert_eq!(chat.scroll.borrow().offset, bottom - 1);
    }

    #[test]
    fn scrollbar_thumb_reaches_track_bottom_when_scrolled_to_bottom() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "hi".into(),
        });
        for i in 0..40 {
            chat.update(ChatMessage::TokenReceived {
                content: format!("line {i}\n"),
            });
        }
        let buf = draw(&chat, 80, 10);
        let bar_x = buf.area().width - 1;
        assert!(
            chat.scroll.borrow().sticky_bottom,
            "streaming keeps the view pinned to the bottom"
        );
        assert_eq!(
            buf[(bar_x, 9)].symbol(),
            "█",
            "thumb covers the last track row"
        );
        assert_eq!(buf[(bar_x, 0)].symbol(), " ", "top of the track is empty");
    }

    #[test]
    fn paint_shows_first_prompt_at_top_after_load() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::Load {
            session: session_with_user_turns(10),
        });
        let buf = draw(&chat, 80, 12);
        assert!(
            buf[(0, 0)].symbol() != "█",
            "no scrollbar at the top row of the track"
        );
        let row_text: String = (0..60).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            row_text.contains("prompt number 0"),
            "top row shows the first prompt, got: {row_text:?}"
        );
    }

    #[test]
    fn width_change_re_renders_window_at_new_width() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::Load {
            session: session_with_user_turns(60),
        });
        draw(&chat, 80, 20);
        assert!(chat.turns.borrow()[0].cache.as_ref().unwrap().width == 79);
        draw(&chat, 40, 20);
        let turns = chat.turns.borrow();
        assert!(
            turns[0]
                .cache
                .as_ref()
                .is_some_and(|cache| cache.width == 39),
            "window re-rendered at the new width"
        );
        assert!(
            turns.last().unwrap().cache.is_none(),
            "far turns stay unrendered after a width change"
        );
    }

    #[test]
    fn big_expanded_tool_body_paints_visible_tail_when_scrolled() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::ToolStarted {
            name: "grep".into(),
            args: serde_json::json!({ "pattern": "x" }),
            worker: None,
            call_id: None,
        });
        let output: String = (0..500)
            .map(|i| format!("out {i}: payload {i} with filler"))
            .collect::<Vec<_>>()
            .join("\n");
        chat.update(ChatMessage::ToolFinished {
            name: "grep".into(),
            ok: true,
            output,
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
            call_id: None,
        });
        chat.update(ChatMessage::ToggleLastTool);
        let buf = draw(&chat, 80, 24);
        let row_text = |y: u16| {
            (0..buf.area().width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        assert!(
            (0..buf.area().height).any(|y| row_text(y).contains("out 499:")),
            "tail rows of the sliced body must paint at sticky bottom"
        );
    }

    #[test]
    fn toggle_expansion_survives_eviction() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::Load {
            session: session_with_tool_turns(30),
        });
        for _ in 0..6000 {
            chat.update(ChatMessage::ScrollDown);
        }
        draw(&chat, 80, 20);
        chat.update(ChatMessage::ToggleLastTool);
        let tool_idx = (0..chat.turns.borrow().len())
            .rev()
            .find(|i| {
                chat.turns.borrow()[*i]
                    .blocks
                    .as_ref()
                    .is_some_and(|blocks| blocks.iter().any(Block::is_tool))
            })
            .unwrap();
        let expanded = {
            let turns = chat.turns.borrow();
            let blocks = turns[tool_idx].blocks.as_ref().unwrap();
            let pos = blocks.iter().rposition(Block::is_tool).unwrap();
            blocks[pos].is_expanded()
        };
        assert!(expanded, "toggled tool block is expanded");

        for _ in 0..6000 {
            chat.update(ChatMessage::ScrollUp);
        }
        draw(&chat, 80, 20);
        assert!(
            chat.turns.borrow()[tool_idx].blocks.is_none(),
            "turn evicted while scrolled away"
        );

        for _ in 0..6000 {
            chat.update(ChatMessage::ScrollDown);
        }
        draw(&chat, 80, 20);
        let turns = chat.turns.borrow();
        let blocks = turns[tool_idx].blocks.as_ref().unwrap();
        let pos = blocks.iter().rposition(Block::is_tool).unwrap();
        assert!(
            blocks[pos].is_expanded(),
            "expansion preserved across eviction"
        );
    }

    #[test]
    fn click_toggles_tool_block() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "check".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolFinished {
            name: "read_file".into(),
            ok: true,
            output: String::new(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
            call_id: None,
        });

        let buf = draw(&chat, 80, 20);
        let rect = chat.history_rect.get();
        let click_row = (1..buf.area().height).find(|row| buf[(0, *row)].bg == theme::SUCCESS_BG);
        let Some(row) = click_row else {
            panic!("tool block background not found");
        };
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: rect.x + 5,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Up,
            column: rect.x + 5,
            row,
        });
        let expanded = chat
            .in_flight
            .borrow()
            .as_ref()
            .unwrap()
            .blocks
            .as_ref()
            .unwrap()
            .last()
            .map(Block::is_expanded);
        assert_eq!(expanded, Some(true));
    }

    /// The buffer row holding `needle`, searched left to right.
    fn row_with(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let line: String = (x..buf.area().width)
                    .map(|cx| buf[(cx, y)].symbol().to_string())
                    .collect::<String>();
                if let Some(col) = line.find(needle) {
                    return Some((x + col as u16, y));
                }
            }
        }
        None
    }

    #[test]
    fn drag_selects_and_copies_plain_text() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "hello brave world".into(),
        });
        let buf = draw(&chat, 80, 20);
        let (col, row) = row_with(&buf, "hello").expect("prompt text on screen");
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: col,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: col + 1,
            row,
        });
        let (end_col, _) = row_with(&buf, "world").expect("tail on screen");
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: end_col + "world".len() as u16,
            row,
        });
        assert_eq!(chat.selected_text().as_deref(), Some("hello brave world"));
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Up,
            column: end_col + "world".len() as u16,
            row,
        });
        assert_eq!(chat.selected_text().as_deref(), Some("hello brave world"));
        assert!(chat.take_pending_copy().is_some(), "drag finalized");
        assert!(chat.take_pending_copy().is_none(), "consumed once");
    }

    #[test]
    fn small_drag_is_a_click_without_selection() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "check".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolFinished {
            name: "read_file".into(),
            ok: true,
            output: String::new(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
            call_id: None,
        });
        let buf = draw(&chat, 80, 20);
        let Some((col, row)) = (1..buf.area().height)
            .find_map(|row| (buf[(0, row)].bg == theme::SUCCESS_BG).then_some((5, row)))
        else {
            panic!("tool block background not found");
        };
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: col,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: col + 1,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Up,
            column: col + 1,
            row,
        });
        assert!(chat.selected_text().is_none(), "slop drag stays a click");
        let expanded = chat
            .in_flight
            .borrow()
            .as_ref()
            .unwrap()
            .blocks
            .as_ref()
            .unwrap()
            .last()
            .map(Block::is_expanded);
        assert_eq!(expanded, Some(true));
    }

    #[test]
    fn selection_cleared_on_session_load() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "hello brave world".into(),
        });
        let buf = draw(&chat, 80, 20);
        let (col, row) = row_with(&buf, "hello").expect("prompt text");
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: col,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: col + 10,
            row,
        });
        assert!(chat.selected_text().is_some());
        chat.update(ChatMessage::Load {
            session: shuvarie_core::Session::default(),
        });
        assert!(chat.selected_text().is_none());
    }

    #[test]
    fn drag_across_wrapped_rows_copies_logical_text() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "first words here and some more words that wrap the row width\nsecond line"
                .into(),
        });
        let buf = draw(&chat, 50, 20);
        let Some((col, row)) = row_with(&buf, "first words") else {
            panic!("first line on screen");
        };
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: col,
            row,
        });
        let Some((end_col, end_row)) = row_with(&buf, "second line") else {
            panic!("second line on screen");
        };
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: end_col + "second line".len() as u16,
            row: end_row,
        });
        assert_eq!(
            chat.selected_text().as_deref(),
            Some("first words here and some more words that wrap the row width\nsecond line")
        );
    }

    #[test]
    fn boundary_drag_copies_visual_fragment() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "alpha beta gamma".into(),
        });
        let buf = draw(&chat, 80, 20);
        let Some((col, row)) = row_with(&buf, "beta") else {
            panic!("text on screen");
        };
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: col,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: col + 4,
            row,
        });
        assert_eq!(chat.selected_text().as_deref(), Some("beta"));
    }

    #[test]
    fn double_click_selects_word() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "alpha beta gamma".into(),
        });
        let buf = draw(&chat, 80, 20);
        let Some((col, row)) = row_with(&buf, "beta") else {
            panic!("text on screen");
        };
        for _ in 0..2 {
            chat.update(ChatMessage::Mouse {
                kind: super::MouseKind::Down,
                column: col,
                row,
            });
        }
        assert_eq!(chat.selected_text().as_deref(), Some("beta"));
    }

    #[test]
    fn triple_click_selects_logical_row() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "alpha beta gamma\nsecond row".into(),
        });
        let buf = draw(&chat, 80, 20);
        let Some((col, row)) = row_with(&buf, "beta") else {
            panic!("text on screen");
        };
        for _ in 0..3 {
            chat.update(ChatMessage::Mouse {
                kind: super::MouseKind::Down,
                column: col,
                row,
            });
        }
        assert_eq!(chat.selected_text().as_deref(), Some("alpha beta gamma"));
    }

    #[test]
    fn fourth_click_starts_a_fresh_point_selection() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "alpha beta gamma".into(),
        });
        let buf = draw(&chat, 80, 20);
        let Some((col, row)) = row_with(&buf, "beta") else {
            panic!("text on screen");
        };
        for _ in 0..4 {
            chat.update(ChatMessage::Mouse {
                kind: super::MouseKind::Down,
                column: col,
                row,
            });
        }
        assert_eq!(chat.selected_text(), None, "count wrapped to point start");
    }

    #[test]
    fn overlay_tints_selection_cells() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "alpha beta gamma".into(),
        });
        let buf = draw(&chat, 80, 20);
        let Some((col, row)) = row_with(&buf, "beta") else {
            panic!("text on screen");
        };
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Down,
            column: col,
            row,
        });
        chat.update(ChatMessage::Mouse {
            kind: super::MouseKind::Drag,
            column: col + 4,
            row,
        });
        let buf = draw(&chat, 80, 20);
        let mut tinted = 0usize;
        for x in 0..buf.area().width {
            if buf[(x, row)].bg == theme::SELECTION {
                tinted += 1;
            }
        }
        assert_eq!(tinted, 4, "boundary row tints exactly the head cols");
    }

    #[test]
    fn bench_spinner_frame_rebuild_cost() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::TokenReceived {
            content: "Exploring the workspace now.".into(),
        });
        let big = {
            let mut s = String::new();
            for i in 0..400 {
                s.push_str(&format!("line {i}: src/module_{i}.rs:12: fn example_{i}(x: &str) -> usize {{ x.len() }}\n"));
            }
            s
        };
        for i in 0..30 {
            chat.update(ChatMessage::ToolStarted {
                name: "read_file".into(),
                args: serde_json::json!({ "path": format!("src/module_{i}.rs") }),
                worker: Some("explore_workspace".into()),
                call_id: Some(format!("call_{i}")),
            });
            chat.update(ChatMessage::ToolFinished {
                name: "read_file".into(),
                ok: true,
                output: big.clone(),
                worker: Some("explore_workspace".into()),
                file_change: None,
                streams: None,
                duration_ms: 12,
                call_id: Some(format!("call_{i}")),
            });
        }
        chat.update(ChatMessage::TokenReceived {
            content: "Here is what I found.".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            worker: None,
            call_id: Some("call_running".into()),
        });
        let diags = BTreeMap::new();
        let env_rev = 0u64;
        let turns_len = chat.turns.borrow().len();
        {
            let mut turn = chat.in_flight.borrow_mut();
            let turn = turn.as_mut().expect("in-flight turn");
            turn.ensure_cache(
                turns_len,
                100,
                &ChatEnv {
                    lsp_diagnostics: &diags,
                    rev: env_rev,
                },
                env_rev,
                TurnFlags {
                    in_flight: true,
                    interrupted_marker: false,
                },
            );
        }
        let rev_before = chat.in_flight.borrow().as_ref().expect("turn").rev;
        let started = std::time::Instant::now();
        let frames = 20;
        for _ in 0..frames {
            chat.update(ChatMessage::SpinnerUpdate);
            let mut turn = chat.in_flight.borrow_mut();
            let turn = turn.as_mut().expect("in-flight turn");
            turn.ensure_cache(
                turns_len,
                100,
                &ChatEnv {
                    lsp_diagnostics: &diags,
                    rev: env_rev,
                },
                env_rev,
                TurnFlags {
                    in_flight: true,
                    interrupted_marker: false,
                },
            );
            assert!(turn.height(100, env_rev) > 0);
        }
        assert_eq!(
            chat.in_flight.borrow().as_ref().expect("turn").rev,
            rev_before,
            "spinner frames must not invalidate the turn cache"
        );
        let per_frame = started.elapsed() / frames;
        println!("spinner-frame repaint: {per_frame:?} per frame (30 blocks x 16KB output)");
        assert!(
            per_frame < std::time::Duration::from_millis(5),
            "repaint too slow: {per_frame:?}"
        );
    }

    #[test]
    fn bench_streaming_delta_cost() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "start".into(),
        });
        chat.update(ChatMessage::TokenReceived {
            content: "Intro paragraph before the long reply.".into(),
        });
        let diags = BTreeMap::new();
        let env_rev = 0u64;
        let turns_len = 0usize;
        let delta = "some streamed prose line that wraps a couple of times at this width\n";
        let deltas = 400;
        // Realistic streaming: paragraph breaks let the markdown cache commit
        // everything before the open paragraph, so a delta re-renders only
        // that open paragraph.
        let started = std::time::Instant::now();
        for i in 0..deltas {
            let mut content = format!("line {i}: {delta}");
            if i % 4 == 3 {
                content.push('\n');
            }
            chat.update(ChatMessage::TokenReceived { content });
            let mut turn = chat.in_flight.borrow_mut();
            let turn = turn.as_mut().expect("in-flight turn");
            turn.ensure_cache(
                turns_len,
                100,
                &ChatEnv {
                    lsp_diagnostics: &diags,
                    rev: env_rev,
                },
                env_rev,
                TurnFlags {
                    in_flight: true,
                    interrupted_marker: false,
                },
            );
            assert!(turn.height(100, env_rev) > 0);
        }
        let per_delta = started.elapsed() / deltas;
        println!("streaming delta: {per_delta:?} per delta (appended + ensured cache)");
        assert!(
            per_delta < std::time::Duration::from_millis(2),
            "delta too slow: {per_delta:?}"
        );
    }

    #[test]
    fn spinner_tick_repaints_without_invalidation() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::TokenReceived {
            content: "Working on it.".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "ls" }),
            worker: None,
            call_id: Some("call_1".into()),
        });
        let diags = BTreeMap::new();
        let env = ChatEnv {
            lsp_diagnostics: &diags,
            rev: 0,
        };
        {
            let mut turn = chat.in_flight.borrow_mut();
            let turn = turn.as_mut().expect("in-flight turn");
            turn.ensure_cache(
                0,
                80,
                &env,
                0,
                TurnFlags {
                    in_flight: true,
                    interrupted_marker: false,
                },
            );
        }
        let rev_before;
        let other_lines;
        let tool_line_before;
        {
            let turn = chat.in_flight.borrow();
            let turn = turn.as_ref().expect("in-flight turn");
            let cache = turn.cache().expect("cache");
            assert!(!cache.spinners.is_empty(), "running tool must hold a slot");
            rev_before = turn.rev;
            let slot = cache.spinners[0].seg;
            tool_line_before = cache.segs[slot].segment.flattened()[0]
                .spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>();
            other_lines = (0..cache.segs.len())
                .filter(|i| *i != slot)
                .map(|i| cache.segs[i].segment.flattened().len())
                .collect::<Vec<_>>();
        }
        chat.update(ChatMessage::SpinnerUpdate);
        let turn = chat.in_flight.borrow();
        let turn = turn.as_ref().expect("in-flight turn");
        assert_eq!(turn.rev, rev_before, "spinner tick must not bump rev");
        let cache = turn.cache().expect("cache");
        let slot = cache.spinners[0].seg;
        let tool_line = cache.segs[slot].segment.flattened()[0]
            .spans
            .iter()
            .map(|span| span.content.clone())
            .collect::<String>();
        assert_eq!(
            tool_line, tool_line_before,
            "same wall-clock frame: same glyph"
        );
        let other_lines_after: Vec<usize> = (0..cache.segs.len())
            .filter(|i| *i != slot)
            .map(|i| cache.segs[i].segment.flattened().len())
            .collect();
        assert_eq!(other_lines, other_lines_after, "non-spinner segs untouched");
        let glyph = crate::tui::spinner::spinner();
        assert!(
            tool_line.starts_with(glyph.content.as_ref()),
            "tool segment must still render the current spinner glyph: {tool_line:?}"
        );
    }

    fn steered_contents(chat: &Chat) -> Vec<String> {
        chat.steered
            .borrow()
            .iter()
            .map(|turn| {
                let blocks = turn.blocks.as_deref().unwrap();
                let Block::Steered(block) = &blocks[0] else {
                    panic!("not a steered block");
                };
                block
                    .view(80)
                    .into_iter()
                    .flat_map(|segment| segment.flattened())
                    .skip(1)
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.clone())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect()
    }

    #[test]
    fn steered_queue_lifecycle() {
        let mut chat = Chat::new();
        assert!(!chat.has_steered());

        chat.update(ChatMessage::SteeredQueued {
            content: "first".into(),
        });
        chat.update(ChatMessage::SteeredQueued {
            content: "second".into(),
        });
        assert_eq!(
            steered_contents(&chat),
            vec!["first".to_string(), "second".into()]
        );

        chat.update(ChatMessage::SteeredDispatched);
        assert_eq!(steered_contents(&chat), vec!["second".to_string()]);

        chat.update(ChatMessage::SteeredRecalled);
        assert!(!chat.has_steered());

        chat.update(ChatMessage::SteeredQueued {
            content: "x".into(),
        });
        chat.update(ChatMessage::SteeredCleared);
        assert!(!chat.has_steered());
    }

    #[test]
    fn steered_entries_render_below_in_flight_turn() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "first prompt".into(),
        });
        chat.update(ChatMessage::TokenReceived {
            content: "streaming answer".into(),
        });
        chat.update(ChatMessage::SteeredQueued {
            content: "queued prompt".into(),
        });
        let buf = draw(&chat, 100, 24);
        let row_text = |y: u16| {
            (0..buf.area().width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        let find = |needle: &str| {
            (0..buf.area().height)
                .map(|y| (y, row_text(y)))
                .find(|(_, text)| text.contains(needle))
                .map(|(y, _)| y)
        };
        let (header, body, stream) = (
            find("steered").expect("steered header should render"),
            find("queued prompt").expect("steered body should render"),
            find("streaming answer").expect("in-flight turn should render"),
        );
        assert!(row_text(header).contains("sends after the current action"));
        assert!(
            body > stream,
            "steered entry renders below the in-flight turn"
        );
    }

    #[test]
    fn interrupted_turn_kills_running_tool_blocks() {
        // The interrupt lands mid-tool: the block must reach a terminal state
        // (killed, keeping its streamed output) instead of vanishing, and the
        // committed turn must stop animating.
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "run_shell".into(),
            args: serde_json::json!({"command": "sleep 30"}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolOutput {
            tool: "run_shell".into(),
            worker: None,
            stdout: "partial output".into(),
            stderr: String::new(),
        });
        chat.update(ChatMessage::StreamCancelled);

        assert!(chat.in_flight.borrow().is_none());
        assert!(
            chat.last_turn_interrupted(),
            "turn committed as interrupted"
        );
        assert!(!chat.has_running_tool_blocks(), "no block animates anymore");
        let turns = chat.turns.borrow();
        let turn = turns.last().unwrap();
        let blocks = turn.blocks.as_deref().unwrap();
        let Block::Tool(tool) = &blocks[0] else {
            panic!("expected the killed tool block");
        };
        assert!(!tool.is_running());
        drop(turns);

        let text = render_turn_lines(&chat, Some(1), 80).unwrap();
        assert!(text.contains("⏹"), "killed marker in the header: {text}");
        assert!(text.contains("killed"), "killed label: {text}");
        assert!(text.contains("partial"), "streamed output kept: {text}");
        assert!(
            text.contains("Took"),
            "killed block shows its duration: {text}"
        );
        assert!(!text.contains("Elapsed "), "the run is over: {text}");
    }

    #[test]
    fn interrupted_text_turn_keeps_thinking_and_text() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ReasoningReceived {
            content: "thinking".into(),
        });
        chat.update(ChatMessage::TokenReceived {
            content: "partial reply".into(),
        });
        chat.update(ChatMessage::StreamError {
            error: "boom".into(),
        });
        let text = render_turn_lines(&chat, Some(1), 80).unwrap();
        assert!(text.contains("Thought"), "thinking finalized: {text}");
        assert!(text.contains("partial reply"));
        assert!(!text.contains("Thinking..."), "no frozen thinking header");
        assert!(chat.last_turn_interrupted());
    }

    #[test]
    fn timeout_killed_shell_finishes_as_killed() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "run_shell".into(),
            args: serde_json::json!({"command": "cargo build", "timeout_secs": 30}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolFinished {
            name: "run_shell".into(),
            ok: false,
            output: "timeout 30s:\nsome partial output".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 30_100,
            call_id: None,
        });
        let text = render_turn_lines(&chat, None, 80).unwrap();
        assert!(
            text.contains("⏹"),
            "timeout kill shows the stop marker: {text}"
        );
        assert!(
            !text.contains("✗"),
            "a timeout kill is not a failure: {text}"
        );
        assert!(
            text.contains("timeout 30s"),
            "the timeout label stays: {text}"
        );
    }

    #[test]
    fn shell_exit_failure_stays_failed() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "go".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "run_shell".into(),
            args: serde_json::json!({"command": "false"}),
            worker: None,
            call_id: None,
        });
        chat.update(ChatMessage::ToolFinished {
            name: "run_shell".into(),
            ok: false,
            output: "exit 1:\nboom".into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 12,
            call_id: None,
        });
        let text = render_turn_lines(&chat, None, 80).unwrap();
        assert!(
            text.contains("✗"),
            "real failure keeps the error marker: {text}"
        );
        assert!(!text.contains("⏹"), "no stop marker on a failure: {text}");
    }
}
