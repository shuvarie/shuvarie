use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use ratatui::prelude::*;
use serde_json::Value;
use shuvarie_core::tool_record::ToolRecord;
use shuvarie_core::{DiagnosticInfo, Role};
use shuvarie_db::ReasoningSegment;
use shuvarie_llm::{FileChange, ShellStreams};

use super::blocks::{
    Block, BlockMessage, ChatEnv, ContextBlock, ReasoningBlock, SystemText, TextBlock, ToolBlock,
    ToolMessage, UserPrompt,
};
use super::segment::BlockAddr;
use super::virtualizer::{TurnData, TurnEst, TurnFlags, locate, paint_turn};
use crate::tui::theme;

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
    },
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
        worker: Option<String>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
        duration_ms: u64,
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
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
        duration_ms: u64,
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
    Click {
        column: u16,
        row: u16,
    },
    Wheel {
        up: bool,
        column: u16,
        row: u16,
    },
    ToggleLastTool,
}

/// Viewport-relative scroll position. `sticky_bottom` tracks the streaming
/// follow state: pinned to the bottom while true, released by any upward
/// scroll and re-engaged when scrolling back down to the last row.
#[derive(Debug, Default, Clone, Copy)]
struct Scroll {
    offset: u32,
    sticky_bottom: bool,
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
    streaming: bool,
    interrupted: bool,
    lsp_diagnostics: BTreeMap<String, Vec<DiagnosticInfo>>,
    stored: Option<shuvarie_core::Session>,
    stored_len: usize,
    scroll: RefCell<Scroll>,
    width: Cell<u16>,
    env_rev: u64,
    toggled: BTreeSet<(usize, usize)>,
    history_rect: Cell<Rect>,
}

impl Chat {
    pub fn new() -> Self {
        Self {
            turns: RefCell::new(Vec::new()),
            in_flight: RefCell::new(None),
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
            history_rect: Cell::new(Rect::default()),
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    pub fn has_messages(&self) -> bool {
        !self.turns.borrow().is_empty()
    }

    pub fn is_interrupted(&self) -> bool {
        self.interrupted
    }

    /// Mark the in-flight turn dirty so an animated spinner re-renders.
    pub fn mark_spinner_dirty(&self) {
        if let Some(turn) = self.in_flight.borrow_mut().as_mut() {
            turn.rev += 1;
        }
    }

    pub fn update(&mut self, msg: ChatMessage) {
        match msg {
            ChatMessage::BeginUserTurn { content } => {
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
            ChatMessage::ToolStarted { name, args, worker } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.push_block(Block::Tool(ToolBlock::new(name, args.to_string(), worker)));
            }
            ChatMessage::ToolFinished {
                name,
                ok,
                output,
                worker,
                file_change,
                streams,
                duration_ms,
            } => {
                let (display_output, display_stderr) = match streams {
                    Some(streams) => (streams.stdout, streams.stderr),
                    None => (output, String::new()),
                };
                self.with_running_tool(&name, &worker, |block| {
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
                let updated = self.with_running_tool(&tool, &worker, |block| {
                    block.update(BlockMessage::Tool(ToolMessage::Output { stdout, stderr }))
                });
                if updated {
                    self.touch_in_flight();
                }
            }
            ChatMessage::WorkerStarted { name, args } => {
                self.streaming = true;
                let mut in_flight = self.in_flight.borrow_mut();
                let turn = in_flight.get_or_insert_with(|| TurnData::new(Role::Assistant));
                turn.push_block(Block::Tool(ToolBlock::new(
                    name,
                    args.to_string(),
                    Some(String::new()),
                )));
            }
            ChatMessage::WorkerFinished {
                name,
                ok,
                output,
                duration_ms,
            } => {
                self.with_running_tool(&name, &Some(String::new()), |block| {
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
                self.stored = None;
                self.stored_len = 0;
                self.streaming = false;
                self.interrupted = false;
                self.toggled.clear();
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
            ChatMessage::Click { column, row } => self.handle_click(column, row),
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
            for slot in turns.iter_mut() {
                slot.cache = None;
            }
            if let Some(turn) = in_flight.as_mut() {
                turn.cache = None;
            }
        }

        let env = ChatEnv {
            lsp_diagnostics: &self.lsp_diagnostics,
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
            if let Some(turn) = in_flight.as_mut() {
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
            }
        }

        self.render_scrollbar(frame, area, scroll_y, total);
        self.evict(&mut turns, scroll_y, viewport);
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

    fn with_running_tool(
        &mut self,
        name: &str,
        worker: &Option<String>,
        apply: impl FnOnce(&mut Block) -> bool,
    ) -> bool {
        let mut in_flight = self.in_flight.borrow_mut();
        let Some(turn) = in_flight.as_mut() else {
            return false;
        };
        let Some(blocks) = turn.blocks.as_mut() else {
            return false;
        };
        match blocks
            .iter_mut()
            .rev()
            .find(|block| block.tool_matches(name, worker) && block.tool_is_running())
        {
            Some(block) => apply(block),
            None => false,
        }
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
        }
        self.streaming = false;
    }

    fn commit_interrupted(&mut self) {
        if let Some(mut turn) = self.in_flight.borrow_mut().take() {
            turn.finish_thinking();
            if let Some(blocks) = turn.blocks.as_mut() {
                blocks.retain(|block| !(block.is_tool() && block.tool_is_running()));
            }
            let has_content = turn.blocks.as_ref().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.is_text() || matches!(block, Block::Reasoning(_) | Block::Tool(_))
                })
            });
            if has_content {
                self.interrupted = true;
                turn.refresh_est(true);
                self.turns.borrow_mut().push(turn);
            }
        }
        self.streaming = false;
    }

    fn apply_session(&mut self, session: shuvarie_core::Session, reset_scroll: bool) {
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
        if let Some(addr) = target {
            self.toggle_block(addr);
        }
    }

    fn toggle_block(&mut self, addr: BlockAddr) {
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
        } else {
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
                blocks.push(Block::Tool(ToolBlock::from_record(record)));
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
            .view()
            .first()
            .and_then(|segment| segment.lines.first())
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
                        .lines
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
        });
        chat.update(ChatMessage::ToolFinished {
            name: "edit_file".into(),
            ok: true,
            output: String::new(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
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
        });
        chat.update(ChatMessage::ToolFinished {
            name: "read_file".into(),
            ok: true,
            output: String::new(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
        });

        let buf = draw(&chat, 80, 20);
        let rect = chat.history_rect.get();
        let click_row = (1..buf.area().height).find(|row| buf[(0, *row)].bg == theme::SUCCESS_BG);
        let Some(row) = click_row else {
            panic!("tool block background not found");
        };
        chat.update(ChatMessage::Click {
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
}
