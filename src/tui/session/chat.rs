use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use ratatui::layout::{Rect, Size};
use ratatui::prelude::*;
use serde_json::Value;
use shuvarie_core::{DiagnosticInfo, Role};
use shuvarie_db::ReasoningSegment;
use shuvarie_llm::{FileChange, ShellStreams};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use super::blocks::{
    Block, BlockMessage, ChatEnv, ContextBlock, ReasoningBlock, ReasoningMessage, SystemText,
    TextBlock, TextMessage, ToolBlock, ToolMessage, UserPrompt,
};
use super::segment::{BlockAddr, HitRegion, Segment};
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

/// One conversation turn: its role and the ordered block models it is made
/// of (context loading, reasoning, markdown chunks interleaved with tool
/// blocks, decorations).
struct Turn {
    role: Role,
    blocks: Vec<Block>,
}

impl Turn {
    fn append_text(&mut self, content: String) {
        self.finish_thinking();
        let last_is_text = matches!(self.blocks.last(), Some(Block::Text(_)));
        if last_is_text && let Some(block) = self.blocks.last_mut() {
            block.update(BlockMessage::Text(TextMessage::Append(content)));
            return;
        }
        self.blocks.push(Block::Text(TextBlock::new(content)));
    }

    fn append_reasoning(&mut self, content: String) {
        let last_is_reasoning = matches!(self.blocks.last(), Some(Block::Reasoning(_)));
        if last_is_reasoning && let Some(block) = self.blocks.last_mut() {
            block.update(BlockMessage::Reasoning(ReasoningMessage::Append(content)));
            return;
        }
        self.blocks
            .push(Block::Reasoning(ReasoningBlock::new(content)));
    }

    fn finish_thinking(&mut self) {
        for block in &mut self.blocks {
            if matches!(block, Block::Reasoning(_)) {
                block.update(BlockMessage::Reasoning(ReasoningMessage::Finish));
            }
        }
    }
}

/// The chat history pane: committed turns, the in-flight streaming turn, the
/// scroll view cache, and the click hit regions. Each turn owns a list of
/// block models; this model stacks, measures, and paints their segments.
pub struct Chat {
    turns: Vec<Turn>,
    in_flight: Option<Turn>,
    streaming: bool,
    interrupted: bool,
    lsp_diagnostics: BTreeMap<String, Vec<DiagnosticInfo>>,
    scroll_state: RefCell<ScrollViewState>,
    scroll_view: RefCell<ScrollView>,
    scroll_dirty: Cell<bool>,
    scroll_width: Cell<u16>,
    committed_scroll: RefCell<ScrollView>,
    committed_dirty: Cell<bool>,
    committed_regions: RefCell<Vec<HitRegion>>,
    hit_regions: RefCell<Vec<HitRegion>>,
    history_rect: Cell<Rect>,
}

impl Chat {
    pub fn new() -> Self {
        Self {
            turns: Vec::new(),
            in_flight: None,
            streaming: false,
            interrupted: false,
            lsp_diagnostics: BTreeMap::new(),
            scroll_state: RefCell::new(ScrollViewState::default()),
            scroll_view: RefCell::new(ScrollView::new(Size::new(0, 0))),
            scroll_dirty: Cell::new(false),
            scroll_width: Cell::new(0),
            committed_scroll: RefCell::new(ScrollView::new(Size::new(0, 0))),
            committed_dirty: Cell::new(true),
            committed_regions: RefCell::new(Vec::new()),
            hit_regions: RefCell::new(Vec::new()),
            history_rect: Cell::new(Rect::default()),
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    pub fn has_messages(&self) -> bool {
        !self.turns.is_empty()
    }

    pub fn is_interrupted(&self) -> bool {
        self.interrupted
    }

    /// Mark the scroll view dirty so an animated spinner re-renders.
    pub fn mark_spinner_dirty(&self) {
        self.scroll_dirty.set(true);
    }

    pub fn update(&mut self, msg: ChatMessage) {
        match msg {
            ChatMessage::BeginUserTurn { content } => {
                self.turns.push(Turn {
                    role: Role::User,
                    blocks: vec![Block::User(UserPrompt::new(content))],
                });
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::TokenReceived { content } => {
                self.ensure_in_flight().append_text(content);
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::ReasoningReceived { content } => {
                self.ensure_in_flight().append_reasoning(content);
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::ContextLoaded { paths } => {
                let turn = self.ensure_in_flight();
                turn.finish_thinking();
                turn.blocks.push(Block::Context(ContextBlock::new(paths)));
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::ToolStarted { name, args, worker } => {
                let turn = self.ensure_in_flight();
                turn.finish_thinking();
                turn.blocks
                    .push(Block::Tool(ToolBlock::new(name, args.to_string(), worker)));
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::ToolFinished {
                name,
                ok,
                output,
                worker,
                file_change,
                streams,
            } => {
                let (display_output, display_stderr) = match streams {
                    Some(streams) => (streams.stdout, streams.stderr),
                    None => (output, String::new()),
                };
                if let Some(block) = self.running_tool_mut(&name, &worker) {
                    block.update(BlockMessage::Tool(ToolMessage::Finish {
                        ok,
                        output: display_output,
                        stderr: display_stderr,
                        file_change,
                    }));
                }
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::ToolOutput {
                tool,
                worker,
                stdout,
                stderr,
            } => {
                let updated = self.running_tool_mut(&tool, &worker).is_some_and(|block| {
                    block.update(BlockMessage::Tool(ToolMessage::Output { stdout, stderr }))
                });
                if updated {
                    self.mark_scroll_dirty();
                    self.follow_bottom();
                }
            }
            ChatMessage::WorkerStarted { name, args } => {
                let turn = self.ensure_in_flight();
                turn.finish_thinking();
                turn.blocks.push(Block::Tool(ToolBlock::new(
                    name,
                    args.to_string(),
                    Some(String::new()),
                )));
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::WorkerFinished { name, ok, output } => {
                if let Some(block) = self.running_tool_mut(&name, &Some(String::new())) {
                    block.update(BlockMessage::Tool(ToolMessage::Finish {
                        ok,
                        output,
                        stderr: String::new(),
                        file_change: None,
                    }));
                }
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::StreamDone => {
                self.commit_done();
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::StreamError { .. } | ChatMessage::StreamCancelled => {
                self.commit_interrupted();
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                self.follow_bottom();
            }
            ChatMessage::Load { session } => self.apply_session(session, true),
            ChatMessage::TurnReverted { session } => self.apply_session(session, true),
            ChatMessage::TurnRestored { session } => self.apply_session(session, false),
            ChatMessage::Reset => {
                self.turns.clear();
                self.in_flight = None;
                self.streaming = false;
                self.interrupted = false;
                self.lsp_diagnostics.clear();
                self.committed_regions.borrow_mut().clear();
                self.hit_regions.borrow_mut().clear();
                *self.scroll_state.borrow_mut() = ScrollViewState::default();
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
            }
            ChatMessage::LspDiagnostics { path, diagnostics } => {
                if diagnostics.is_empty() {
                    self.lsp_diagnostics.remove(&path);
                } else {
                    self.lsp_diagnostics.insert(path, diagnostics);
                }
                // Diagnostics feed the block view env, so committed tool
                // blocks re-render too.
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
            }
            ChatMessage::ScrollUp => {
                self.scroll_state.borrow_mut().scroll_up();
            }
            ChatMessage::ScrollDown => {
                self.scroll_state.borrow_mut().scroll_down();
            }
            ChatMessage::Click { column, row } => self.handle_click(column, row),
            ChatMessage::Wheel { up, column, row } => {
                if self.in_history(column, row) {
                    let mut state = self.scroll_state.borrow_mut();
                    for _ in 0..3 {
                        if up {
                            state.scroll_up();
                        } else {
                            state.scroll_down();
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
        if self.scroll_width.get() != content_width {
            self.scroll_dirty.set(true);
            self.committed_dirty.set(true);
            self.scroll_width.set(content_width);
        }
        if self.scroll_dirty.replace(false) {
            self.rebuild_scroll_view(content_width);
        }

        let mut scroll_state = self.scroll_state.borrow_mut();
        let scroll_view = self.scroll_view.borrow();
        frame.render_stateful_widget(&*scroll_view, area, &mut scroll_state);

        let content_height = scroll_view.size().height;
        if content_height > area.height {
            self.render_scrollbar(frame, area, &scroll_state, content_height);
        }
    }

    fn ensure_in_flight(&mut self) -> &mut Turn {
        self.streaming = true;
        self.in_flight.get_or_insert_with(|| Turn {
            role: Role::Assistant,
            blocks: Vec::new(),
        })
    }

    fn running_tool_mut(&mut self, name: &str, worker: &Option<String>) -> Option<&mut Block> {
        let turn = self.in_flight.as_mut()?;
        turn.blocks
            .iter_mut()
            .rev()
            .find(|block| block.tool_matches(name, worker) && block.tool_is_running())
    }

    fn commit_done(&mut self) {
        if let Some(mut turn) = self.in_flight.take() {
            turn.finish_thinking();
            self.turns.push(turn);
            self.interrupted = false;
        }
        self.streaming = false;
    }

    fn commit_interrupted(&mut self) {
        if let Some(mut turn) = self.in_flight.take() {
            turn.finish_thinking();
            turn.blocks
                .retain(|block| !(block.is_tool() && block.tool_is_running()));
            let has_content = turn.blocks.iter().any(|block| {
                block.is_text() || matches!(block, Block::Reasoning(_) | Block::Tool(_))
            });
            if has_content {
                self.turns.push(turn);
                self.interrupted = true;
            }
        }
        self.streaming = false;
    }

    fn apply_session(&mut self, session: shuvarie_core::Session, reset_scroll: bool) {
        self.turns = build_turns(&session);
        self.in_flight = None;
        self.streaming = false;
        self.interrupted = session.last_assistant_interrupted();
        self.committed_regions.borrow_mut().clear();
        self.hit_regions.borrow_mut().clear();
        if reset_scroll {
            *self.scroll_state.borrow_mut() = ScrollViewState::default();
        }
        self.mark_committed_dirty();
        self.mark_scroll_dirty();
    }

    fn mark_scroll_dirty(&self) {
        self.scroll_dirty.set(true);
    }

    fn mark_committed_dirty(&self) {
        self.committed_dirty.set(true);
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
        let content_y = self
            .scroll_state
            .borrow()
            .offset()
            .y
            .saturating_add(row - self.history_rect.get().y);
        let addr = self
            .hit_regions
            .borrow()
            .iter()
            .find(|region| content_y >= region.start && content_y < region.end)
            .map(|region| region.addr.clone());
        if let Some(addr) = addr {
            self.toggle_block(addr);
        }
    }

    fn toggle_block(&mut self, addr: BlockAddr) {
        let changed = if addr.turn < self.turns.len() {
            self.turns[addr.turn]
                .blocks
                .get_mut(addr.block)
                .is_some_and(|block| block.update(BlockMessage::Toggle))
        } else {
            self.in_flight
                .as_mut()
                .and_then(|turn| turn.blocks.get_mut(addr.block))
                .is_some_and(|block| block.update(BlockMessage::Toggle))
        };
        if changed {
            if addr.turn < self.turns.len() {
                self.mark_committed_dirty();
            }
            self.mark_scroll_dirty();
        }
    }

    fn toggle_last_tool(&mut self) {
        if let Some(turn) = self.in_flight.as_ref()
            && let Some(block) = turn.blocks.iter().rposition(|block| block.is_tool())
        {
            let addr = BlockAddr {
                turn: self.turns.len(),
                block,
            };
            self.toggle_block(addr);
            return;
        }
        for turn in (0..self.turns.len()).rev() {
            if let Some(block) = self.turns[turn]
                .blocks
                .iter()
                .rposition(|block| block.is_tool())
            {
                self.toggle_block(BlockAddr { turn, block });
                return;
            }
        }
    }

    fn turn_segments(
        &self,
        turn: &Turn,
        turn_idx: usize,
        in_flight: bool,
        width: u16,
    ) -> Vec<Segment> {
        let env = ChatEnv {
            lsp_diagnostics: &self.lsp_diagnostics,
        };
        let mut segments: Vec<Segment> = Vec::new();
        for (block_idx, block) in turn.blocks.iter().enumerate() {
            if block.is_tool() && segments.last().is_some_and(|last| last.bg.is_some()) {
                segments.push(Segment::spacer());
            }
            let mut block_segments = block.view(width, &env);
            let addr = BlockAddr {
                turn: turn_idx,
                block: block_idx,
            };
            match block {
                Block::Tool(_) => {
                    for segment in &mut block_segments {
                        segment.hit = Some(addr.clone());
                    }
                }
                Block::Reasoning(_) => {
                    if let Some(first) = block_segments.first_mut() {
                        first.hit = Some(addr);
                    }
                }
                _ => {}
            }
            segments.extend(block_segments);
        }
        let thinking_now = in_flight && turn.blocks.last().is_some_and(Block::is_thinking);
        if turn.role == Role::Assistant && !turn.blocks.iter().any(Block::is_text) && !thinking_now
        {
            let placeholder = if in_flight {
                Block::Working
            } else {
                Block::ToolOnlyNote
            };
            segments.extend(placeholder.view(width, &env));
        }
        if self.interrupted
            && !self.streaming
            && turn_idx + 1 == self.turns.len()
            && turn.role == Role::Assistant
        {
            segments.extend(Block::Interrupted.view(width, &env));
        }
        segments
    }

    /// Paint `segments` into `sv` from `start_y`, bounded by `total_height`,
    /// recording hit regions as they are stamped.
    fn paint_segments(
        segments: &[Segment],
        sv: &mut ScrollView,
        start_y: u16,
        total_height: u16,
        content_width: u16,
        regions: &mut Vec<HitRegion>,
    ) {
        let mut y = start_y;
        for segment in segments {
            let h = (segment.measure(content_width) as u16).min(total_height.saturating_sub(y));
            if h == 0 {
                continue;
            }
            segment.view(sv, y, h, content_width, regions);
            y = y.saturating_add(h);
        }
    }

    /// Rebuild the scroll view contents: the committed buffer only re-renders
    /// when history changed; the in-flight tail re-renders on every
    /// scroll-dirty frame — in place into the cached combined buffer when its
    /// height is unchanged (no re-allocation, no committed cell copy), and via
    /// a full rebuild when the tail height changed or history was rebuilt.
    fn rebuild_scroll_view(&self, content_width: u16) {
        let committed_was_dirty = self.committed_dirty.replace(false);
        if committed_was_dirty {
            let mut segments: Vec<Segment> = Vec::new();
            for (turn_idx, turn) in self.turns.iter().enumerate() {
                segments.extend(self.turn_segments(turn, turn_idx, false, content_width));
                segments.push(Segment::spacer());
            }
            let total: usize = segments.iter().map(|s| s.measure(content_width)).sum();
            let total = total.min(u16::MAX as usize) as u16;
            let mut sv = ScrollView::new(Size::new(content_width, total))
                .scrollbars_visibility(ScrollbarVisibility::Never);
            let mut regions = Vec::new();
            Self::paint_segments(&segments, &mut sv, 0, total, content_width, &mut regions);
            *self.committed_scroll.borrow_mut() = sv;
            *self.committed_regions.borrow_mut() = regions;
        }

        let committed_scroll = self.committed_scroll.borrow();
        let committed_height = committed_scroll.size().height;

        let mut tail_segments: Vec<Segment> = Vec::new();
        if let Some(turn) = &self.in_flight {
            let turn_idx = self.turns.len();
            tail_segments.extend(self.turn_segments(turn, turn_idx, true, content_width));
            tail_segments.push(Segment::spacer());
        }

        let tail_total: usize = tail_segments.iter().map(|s| s.measure(content_width)).sum();
        let tail_total =
            tail_total.min((u16::MAX as usize).saturating_sub(committed_height as usize)) as u16;
        let total_height = committed_height.saturating_add(tail_total);
        let tail_height_unchanged = self.scroll_view.borrow().size().height == total_height;

        let mut tail_regions = Vec::new();
        if !committed_was_dirty && tail_height_unchanged {
            // Only the in-flight tail changed: clear the stale tail rows and
            // repaint them into the cached combined buffer, skipping the
            // buffer re-allocation and the committed cell copy.
            let mut sv = self.scroll_view.borrow_mut();
            {
                let buf = sv.buf_mut();
                for y in committed_height..total_height {
                    for x in 0..content_width {
                        buf[(x, y)].reset();
                    }
                }
            }
            Self::paint_segments(
                &tail_segments,
                &mut sv,
                committed_height,
                total_height,
                content_width,
                &mut tail_regions,
            );
        } else {
            let mut scroll_view = ScrollView::new(Size::new(content_width, total_height))
                .scrollbars_visibility(ScrollbarVisibility::Never);

            {
                let src = committed_scroll.buf();
                let dst = scroll_view.buf_mut();
                for y in 0..committed_height {
                    for x in 0..content_width {
                        dst[(x, y)] = src[(x, y)].clone();
                    }
                }
            }

            Self::paint_segments(
                &tail_segments,
                &mut scroll_view,
                committed_height,
                total_height,
                content_width,
                &mut tail_regions,
            );

            *self.scroll_view.borrow_mut() = scroll_view;
        }

        let mut regions = self.committed_regions.borrow().clone();
        regions.append(&mut tail_regions);
        *self.hit_regions.borrow_mut() = regions;
    }

    fn render_scrollbar(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        state: &ScrollViewState,
        content_height: u16,
    ) {
        let track_len = area.height as usize;
        if track_len < 2 {
            return;
        }
        let content_len = content_height as usize;
        let viewport_len = area.height as usize;
        let offset = state.offset().y as usize;
        let max_offset = content_len.saturating_sub(viewport_len);
        let max_start = track_len.saturating_sub(1);
        let thumb_len = max_start.max(1) * viewport_len / content_len.max(1);
        let thumb_len = thumb_len.clamp(1, max_start);
        let thumb_start = max_start
            .saturating_sub(thumb_len)
            .saturating_mul(offset)
            .div_ceil(max_offset.max(1))
            .min(max_start.saturating_sub(thumb_len));
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

    fn follow_bottom(&self) {
        let mut state = self.scroll_state.borrow_mut();
        if state.is_at_bottom() {
            state.scroll_to_bottom();
        }
    }
}

/// Rebuild the committed turns from a stored session: user/system messages
/// become their single blocks, assistant messages assemble reasoning, summary
/// marker, tool blocks (in record order, before the text — matching reload
/// layout), and the text chunk.
fn build_turns(session: &shuvarie_core::Session) -> Vec<Turn> {
    let mut turns = Vec::new();
    for (idx, message) in session.messages.iter().enumerate() {
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
                let drain_reasoning = |blocks: &mut Vec<Block>,
                                       seg_i: &mut usize,
                                       tools_done: usize| {
                    while let Some(seg) = segments.get(*seg_i)
                        && (seg.after_tool as usize) <= tools_done
                    {
                        blocks.push(Block::Reasoning(ReasoningBlock::finished(seg.text.clone())));
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
                    blocks.push(Block::Tool(ToolBlock::from_record(
                        record.name.clone(),
                        record.args_json.clone(),
                        record.output.clone(),
                        record.stderr.clone(),
                        record.ok,
                        record.worker.clone(),
                        record.file_change.clone(),
                    )));
                    drain_reasoning(&mut blocks, &mut seg_i, count + 1);
                }
                drain_reasoning(&mut blocks, &mut seg_i, usize::MAX);
                if !message.content.is_empty() {
                    blocks.push(Block::Text(TextBlock::new(message.content.clone())));
                }
            }
        }
        turns.push(Turn {
            role: message.role,
            blocks,
        });
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_core::tool_record::ToolRecord;

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

    fn block_tags(turn: &Turn) -> Vec<&'static str> {
        turn.blocks
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
        }
    }

    #[test]
    fn working_placeholder_hidden_while_thinking() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::ReasoningReceived {
            content: "hmm".into(),
        });
        let render = |chat: &Chat| {
            let turn = chat.in_flight.as_ref().unwrap();
            chat.turn_segments(turn, 0, true, 80)
                .iter()
                .flat_map(|segment| {
                    segment
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
                .join("\n")
        };
        let thinking = render(&chat);
        assert!(thinking.contains("Thinking..."));
        assert!(
            !thinking.contains("(Working...)",),
            "no placeholder while thinking"
        );

        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
        });
        let tool_only = render(&chat);
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

        let done = header_text(&Block::Reasoning(ReasoningBlock::finished("hmm")));
        assert!(done.contains("Thought"), "reloaded header: {done}");
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
        let blocks = &chat.in_flight.as_ref().unwrap().blocks;
        assert_eq!(block_tags(chat.in_flight.as_ref().unwrap()), vec!["R", "X"]);
        assert!(header_text(&blocks[0]).contains("Thought"));

        chat.update(ChatMessage::ReasoningReceived {
            content: "more".into(),
        });
        chat.update(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
        });
        let turn = chat.in_flight.as_ref().unwrap();
        assert_eq!(block_tags(turn), vec!["R", "X", "R", "T"]);
        assert!(header_text(&turn.blocks[0]).contains("Thought"));
        assert!(header_text(&turn.blocks[2]).contains("Thought"));

        chat.update(ChatMessage::ReasoningReceived {
            content: "again".into(),
        });
        let turn = chat.in_flight.as_ref().unwrap();
        assert!(header_text(&turn.blocks[4]).contains("Thinking..."));
    }

    #[test]
    fn commit_finishes_thinking() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::ReasoningReceived {
            content: "only thoughts".into(),
        });
        chat.update(ChatMessage::StreamDone);
        let turn = chat.turns.last().unwrap();
        assert_eq!(block_tags(turn), vec!["R"]);
        assert!(header_text(&turn.blocks[0]).contains("Thought"));
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
                },
                ReasoningSegment {
                    after_tool: 2,
                    text: "after two tools".to_string(),
                },
                ReasoningSegment {
                    after_tool: 9,
                    text: "beyond records".to_string(),
                },
            ],
        );
        session.tool_records = vec![tool_record(1), tool_record(1)];

        let turns = build_turns(&session);
        assert_eq!(block_tags(&turns[1]), vec!["R", "T", "T", "R", "R", "X"]);
        assert!(header_text(&turns[1].blocks[0]).contains("Thought"));
    }

    #[test]
    fn in_place_tail_rebuild_matches_full_rebuild() {
        let mut chat = Chat::new();
        chat.update(ChatMessage::BeginUserTurn {
            content: "make it so".into(),
        });
        chat.rebuild_scroll_view(80);

        chat.update(ChatMessage::TokenReceived {
            content: "hello".into(),
        });
        chat.rebuild_scroll_view(80);

        let total = chat.scroll_view.borrow().size().height;
        chat.scroll_view.borrow_mut().buf_mut()[(0, total - 1)].set_symbol("ZZZZZ");

        assert!(!chat.committed_dirty.get());
        chat.update(ChatMessage::TokenReceived {
            content: " world".into(),
        });
        chat.rebuild_scroll_view(80);

        let in_place = chat.scroll_view.borrow().buf().clone();
        let regions = chat.hit_regions.borrow().len();

        chat.committed_dirty.set(true);
        chat.rebuild_scroll_view(80);

        assert_eq!(
            chat.scroll_view.borrow().size().height,
            total,
            "tail height must stay unchanged for the in-place path"
        );
        assert_eq!(&in_place, chat.scroll_view.borrow().buf());
        assert_eq!(regions, chat.hit_regions.borrow().len());
    }

    #[test]
    fn diagnostics_update_rebuilds_committed_rows() {
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
        });
        chat.update(ChatMessage::StreamDone);
        chat.rebuild_scroll_view(80);

        chat.scroll_view.borrow_mut().buf_mut()[(0, 0)].set_symbol("ZZZZZ");

        chat.update(ChatMessage::LspDiagnostics {
            path: "src/lib.rs".into(),
            diagnostics: vec![],
        });
        chat.rebuild_scroll_view(80);

        let after = chat.scroll_view.borrow().buf().clone();
        chat.committed_dirty.set(true);
        chat.rebuild_scroll_view(80);
        assert_eq!(&after, chat.scroll_view.borrow().buf());
    }
}
