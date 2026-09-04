use std::collections::BTreeSet;

use ratatui::prelude::*;
use shuvarie_core::Role;
use shuvarie_core::tool_record::ToolRecord;
use shuvarie_core::tools::todos::parse_items;
use shuvarie_llm::{FileChange, PatchFileKind};
use unicode_width::UnicodeWidthStr;

use super::blocks::{
    Block, BlockMessage, ChatEnv, ReasoningBlock, ReasoningMessage, TextBlock, TextMessage,
};
use super::segment::{BLOCK_PADDING, BlockAddr, HitRegion, Segment, TEXT_PADDING};

/// Estimated row counters of one turn. Collected once (from stored session
/// data at load, or from block state at materialization/mutation) so heights
/// re-estimate in O(1) per width change without touching markdown rendering.
#[derive(Debug, Default, Clone, Copy)]
pub struct TurnEst {
    pub text_lines: u32,
    pub text_width: u32,
    pub tool_count: u32,
    pub tool_rows: u32,
    pub tool_header_width: u32,
    pub reasoning_rows: u32,
    pub deco_rows: u32,
    pub padding_rows: u32,
}

impl TurnEst {
    pub fn add(&mut self, other: &Self) {
        self.text_lines += other.text_lines;
        self.text_width += other.text_width;
        self.tool_count += other.tool_count;
        self.tool_rows += other.tool_rows;
        self.tool_header_width += other.tool_header_width;
        self.reasoning_rows += other.reasoning_rows;
        self.deco_rows += other.deco_rows;
        self.padding_rows += other.padding_rows;
    }

    pub fn deco(rows: u32) -> Self {
        Self {
            deco_rows: rows,
            ..Self::default()
        }
    }

    pub fn add_text(&mut self, content: &str) {
        for line in content.lines() {
            self.text_lines += 1;
            self.text_width += UnicodeWidthStr::width(line) as u32;
        }
    }

    /// Height in rows at `width` (content width, excluding the scrollbar
    /// column). Approximate: refined to exact rows when the turn is rendered.
    pub fn height(&self, width: u16) -> u32 {
        let w = u32::from(width.max(1));
        let inner = w.saturating_sub(2 * u32::from(BLOCK_PADDING.0)).max(1);
        let text_wrap = self.text_lines.max(self.text_width.div_ceil(w));
        let tool_wrap = self.tool_count.max(self.tool_header_width.div_ceil(inner));
        self.padding_rows
            .saturating_add(text_wrap)
            .saturating_add(tool_wrap)
            .saturating_add(self.tool_rows)
            .saturating_add(self.reasoning_rows)
            .saturating_add(self.deco_rows)
    }

    /// Build the estimate of a stored session message from its raw data —
    /// the lazy path: blocks are not materialized yet.
    pub fn from_session_parts(
        role: Role,
        content: &str,
        tools: &[&ToolRecord],
        reasoning_count: usize,
        summary_marker: bool,
        interrupted_marker: bool,
    ) -> Self {
        let mut est = Self::default();
        match role {
            Role::User => {
                est.padding_rows = 2 * u32::from(BLOCK_PADDING.1);
                est.add_text(content);
            }
            Role::System => {
                est.deco_rows = 1;
                est.add_text(content);
            }
            Role::Assistant => {
                est.reasoning_rows = reasoning_count as u32 * (1 + 2 * u32::from(BLOCK_PADDING.1));
                if summary_marker {
                    est.deco_rows += 1;
                }
                let mut prev_tool = false;
                for record in tools {
                    if prev_tool {
                        est.deco_rows += 1;
                    }
                    prev_tool = true;
                    est.add_tool(record);
                }
                if content.is_empty() {
                    est.deco_rows += 1;
                } else {
                    est.padding_rows += 2 * u32::from(TEXT_PADDING.1);
                    est.add_text(content);
                }
                if interrupted_marker {
                    est.deco_rows += 1;
                }
            }
        }
        est.deco_rows += 1;
        est
    }

    fn add_tool(&mut self, record: &ToolRecord) {
        self.tool_count += 1;
        self.padding_rows += 2 * u32::from(BLOCK_PADDING.1);
        self.tool_header_width += u32::try_from(record.name.chars().count() + 1)
            .unwrap_or(u32::MAX)
            .saturating_add(UnicodeWidthStr::width(record.args_json.as_str()).min(120) as u32);
        self.tool_rows += output_row_est(record);
        if let Some(change) = &record.file_change {
            self.tool_rows += file_change_row_est(change);
        }
        self.tool_rows += 1;
    }
}

/// Collapsed-visible output rows of a persisted tool call. `question` blocks
/// reload collapsed (no body rows); `todo` shows the tail rows like other
/// tools; `run_shell` prefers the stderr tail for display.
fn output_row_est(record: &ToolRecord) -> u32 {
    match record.name.as_str() {
        "question" => 0,
        "todo" => {
            let rows = parse_items(&record.output).map_or(0, |items| items.len() as u32);
            collapsed_rows(rows)
        }
        name => {
            let display = if name == "run_shell" && !record.stderr.is_empty() {
                &record.stderr
            } else {
                &record.output
            };
            collapsed_rows(display.lines().count() as u32)
        }
    }
}

/// Rows a collapsed tool body shows: the last five rows plus a `… +N` hint.
pub(super) fn collapsed_rows(rows: u32) -> u32 {
    rows.min(5) + u32::from(rows > 5)
}

pub(super) fn file_change_row_est(change: &FileChange) -> u32 {
    match change {
        FileChange::Edit { diff, .. } => 1 + diff.len() as u32,
        FileChange::Write { content, .. } => 1 + content.lines().count() as u32,
        FileChange::Patch { files } => files
            .iter()
            .map(|file| {
                1 + match (&file.kind, &file.moved_to) {
                    (PatchFileKind::Add, _) => {
                        file.new.as_deref().unwrap_or_default().lines().count() as u32
                    }
                    (PatchFileKind::Update, Some(_)) => {
                        file.new.as_deref().unwrap_or_default().lines().count() as u32
                    }
                    (PatchFileKind::Update, None) => file.diff.len() as u32,
                    (PatchFileKind::Delete, _) => 0,
                }
            })
            .sum(),
    }
}

/// One projected segment of a turn with its measured height and the row
/// offset at which it starts within the turn.
pub struct TurnSeg {
    pub segment: Segment,
    pub start: u32,
    pub height: u32,
}

/// A rendered turn: its projected segments with per-segment layout, total
/// height, and hit regions — valid for one (width, block revision, env
/// revision) combination.
pub struct TurnCache {
    pub width: u16,
    pub rev: u64,
    pub env_rev: u64,
    pub segs: Vec<TurnSeg>,
    pub height: u32,
    pub hits: Vec<HitRegion>,
}

impl TurnCache {
    pub fn matches(&self, width: u16, rev: u64, env_relevant: bool, env_rev: u64) -> bool {
        self.width == width && self.rev == rev && (!env_relevant || self.env_rev == env_rev)
    }
}

/// Per-turn decoration switches used at render time.
#[derive(Debug, Clone, Copy)]
pub struct TurnFlags {
    pub in_flight: bool,
    pub interrupted_marker: bool,
}

/// One conversation turn in the virtualized chat: either a live TEA block
/// list (`blocks`), a lazy slot backed by the stored session (`blocks: None`,
/// height estimated from `est`), or anything in between (materialized but not
/// currently rendered). Block mutations bump `rev`, which invalidates the
/// rendered cache.
pub struct TurnData {
    pub role: Role,
    pub blocks: Option<Vec<Block>>,
    pub est: TurnEst,
    pub cache: Option<TurnCache>,
    /// Last exactly measured layout height at a given content width. Kept
    /// across cache invalidation and eviction so a turn's layout height never
    /// flips back to the rough estimate — estimate↔exact flapping made the
    /// total (and with it the scroll position) jump around while streaming.
    measured: Option<(u16, u32)>,
    pub rev: u64,
    pub env_relevant: bool,
}

impl TurnData {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            blocks: Some(Vec::new()),
            est: TurnEst::default(),
            cache: None,
            measured: None,
            rev: 0,
            env_relevant: false,
        }
    }

    /// A lazy slot backed by the stored session: no blocks, height from the
    /// precomputed estimate.
    pub fn lazy(role: Role, est: TurnEst) -> Self {
        Self {
            role,
            blocks: None,
            env_relevant: est.tool_count > 0,
            est,
            cache: None,
            measured: None,
            rev: 0,
        }
    }

    /// Install a live block list (streaming paths) and refresh the estimate.
    pub fn set_blocks(&mut self, blocks: Vec<Block>, interrupted_marker: bool) {
        self.blocks = Some(blocks);
        self.refresh_est(interrupted_marker);
    }

    /// Materialize the turn's blocks and apply the persisted expansion
    /// side-set so blocks evicted while expanded come back expanded.
    pub fn materialize(
        &mut self,
        build: impl FnOnce() -> Vec<Block>,
        toggled: &BTreeSet<(usize, usize)>,
        turn_idx: usize,
        interrupted_marker: bool,
    ) {
        let mut blocks = build();
        for (idx, block) in blocks.iter_mut().enumerate() {
            if toggled.contains(&(turn_idx, idx)) {
                block.set_expanded(true);
            }
        }
        self.blocks = Some(blocks);
        self.refresh_est(interrupted_marker);
    }

    pub fn refresh_est(&mut self, interrupted_marker: bool) {
        let mut est = TurnEst::default();
        let mut prev_tool = false;
        let mut has_text = false;
        for block in self.blocks.iter().flatten() {
            let is_tool = block.is_tool();
            if is_tool && prev_tool {
                est.deco_rows += 1;
            }
            prev_tool = is_tool;
            has_text |= block.is_text();
            est.add(&block.est());
        }
        if self.role == Role::Assistant && !has_text {
            est.deco_rows += 1;
        }
        if interrupted_marker && self.role == Role::Assistant {
            est.deco_rows += 1;
        }
        est.deco_rows += 1;
        self.est = est;
        self.env_relevant = est.tool_count > 0;
    }

    pub fn append_text(&mut self, content: String) {
        let appends = matches!(
            self.blocks.as_deref().and_then(|blocks| blocks.last()),
            Some(Block::Text(_))
        );
        if appends {
            if let Some(block) = self.blocks.as_mut().and_then(|blocks| blocks.last_mut()) {
                block.update(BlockMessage::Text(TextMessage::Append(content)));
            }
        } else {
            self.finish_thinking();
            if let Some(blocks) = self.blocks.as_mut() {
                blocks.push(Block::Text(TextBlock::new(content)));
            }
        }
        self.rev += 1;
        self.refresh_est(false);
    }

    pub fn append_reasoning(&mut self, content: String) {
        let appends = matches!(
            self.blocks.as_deref().and_then(|blocks| blocks.last()),
            Some(Block::Reasoning(_))
        );
        if appends {
            if let Some(block) = self.blocks.as_mut().and_then(|blocks| blocks.last_mut()) {
                block.update(BlockMessage::Reasoning(ReasoningMessage::Append(content)));
            }
        } else if let Some(blocks) = self.blocks.as_mut() {
            blocks.push(Block::Reasoning(ReasoningBlock::new(content)));
        }
        self.rev += 1;
        self.refresh_est(false);
    }

    pub fn push_block(&mut self, block: Block) {
        if let Some(blocks) = self.blocks.as_mut() {
            blocks.push(block);
        }
        self.finish_thinking();
        self.rev += 1;
        self.refresh_est(false);
    }

    pub fn finish_thinking(&mut self) {
        for block in self.blocks.iter_mut().flatten() {
            if matches!(block, Block::Reasoning(_)) {
                block.update(BlockMessage::Reasoning(ReasoningMessage::Finish));
            }
        }
    }

    /// Current layout height: exact from a fresh render cache, otherwise the
    /// last measurement taken at this width, otherwise the O(1) estimate.
    /// The measurement fallback keeps heights stable when the cache is stale
    /// (spinner tick, diagnostics pump) or evicted — only turns never
    /// rendered at the current width use the estimate.
    pub fn height(&self, width: u16, env_rev: u64) -> u32 {
        if let Some(cache) = &self.cache
            && cache.matches(width, self.rev, self.env_relevant, env_rev)
        {
            return cache.height;
        }
        if let Some((measured_width, height)) = self.measured
            && measured_width == width
        {
            return height;
        }
        self.est.height(width)
    }

    pub fn cache_is_stale(&self, width: u16, env_rev: u64) -> bool {
        match &self.cache {
            Some(cache) => !cache.matches(width, self.rev, self.env_relevant, env_rev),
            None => true,
        }
    }

    pub fn ensure_cache(
        &mut self,
        turn_idx: usize,
        width: u16,
        env: &ChatEnv,
        env_rev: u64,
        flags: TurnFlags,
    ) {
        if self.blocks.is_none() {
            return;
        }
        if self.cache_is_stale(width, env_rev) {
            let cache = render_turn_cache(self, turn_idx, flags, width, env, env_rev);
            if let Some(cache) = &cache {
                self.measured = Some((width, cache.height));
            }
            self.cache = cache;
        }
    }

    pub fn cache(&self) -> Option<&TurnCache> {
        self.cache.as_ref()
    }
}

/// Render the turn's blocks into segments with per-segment layout: spacer
/// rows between consecutive backgrounded blocks, the working/tool-only
/// placeholder for text-less assistant turns, the interrupted marker, and a
/// trailing spacer separating turns. Hit regions are recorded here so clicks
/// resolve without re-rendering.
pub fn render_turn_cache(
    turn: &TurnData,
    turn_idx: usize,
    flags: TurnFlags,
    width: u16,
    env: &ChatEnv,
    env_rev: u64,
) -> Option<TurnCache> {
    let blocks = turn.blocks.as_deref()?;
    let mut segs: Vec<TurnSeg> = Vec::new();
    let mut hits: Vec<HitRegion> = Vec::new();
    let mut y = 0u32;
    let mut prev_bg = false;
    for (block_idx, block) in blocks.iter().enumerate() {
        if block.is_tool() && prev_bg {
            y = push_segment(&mut segs, Segment::spacer(), y, width);
            prev_bg = false;
        }
        let addr = BlockAddr {
            turn: turn_idx,
            block: block_idx,
        };
        for (i, mut segment) in block.view(width, env).into_iter().enumerate() {
            match block {
                Block::Tool(_) => segment.hit = Some(addr.clone()),
                Block::Reasoning(_) if i == 0 => segment.hit = Some(addr.clone()),
                _ => {}
            }
            let height = segment.measure(width);
            if segment.hit.is_some() {
                hits.push(HitRegion {
                    start: y,
                    end: y + height,
                    addr: addr.clone(),
                });
            }
            prev_bg = segment.bg.is_some();
            y = push_segment(&mut segs, segment, y, width);
        }
    }
    let has_text = blocks.iter().any(Block::is_text);
    let thinking_now = flags.in_flight && blocks.last().is_some_and(Block::is_thinking);
    if turn.role == Role::Assistant && !has_text && !thinking_now {
        let placeholder = if flags.in_flight {
            Block::Working
        } else {
            Block::ToolOnlyNote
        };
        for segment in placeholder.view(width, env) {
            y = push_segment(&mut segs, segment, y, width);
        }
    }
    if flags.interrupted_marker && turn.role == Role::Assistant {
        for segment in Block::Interrupted.view(width, env) {
            y = push_segment(&mut segs, segment, y, width);
        }
    }
    y = push_segment(&mut segs, Segment::spacer(), y, width);
    Some(TurnCache {
        width,
        rev: turn.rev,
        env_rev,
        segs,
        height: y,
        hits,
    })
}

fn push_segment(segs: &mut Vec<TurnSeg>, segment: Segment, y: u32, width: u16) -> u32 {
    let height = segment.measure(width);
    segs.push(TurnSeg {
        segment,
        start: y,
        height,
    });
    y + height
}

/// Paint the visible rows of a rendered turn: `turn_y` is the turn's content
/// offset and `scroll_y` the viewport top, both in content rows.
pub fn paint_turn(
    cache: &TurnCache,
    turn_y: u32,
    scroll_y: u32,
    clip: Rect,
    content_width: u16,
    buf: &mut Buffer,
) {
    let viewport_bottom = scroll_y + u32::from(clip.height);
    for seg in &cache.segs {
        let top = turn_y + seg.start;
        if top >= viewport_bottom {
            break;
        }
        if top + seg.height <= scroll_y {
            continue;
        }
        seg.segment
            .paint(clip, scroll_y, top, seg.height, content_width, buf);
    }
}

/// Map a content-space row to its turn index and the offset within that turn.
pub fn locate(heights: &[u32], y: u32) -> (usize, u32) {
    let mut start = 0u32;
    for (i, h) in heights.iter().enumerate() {
        let end = start + *h;
        if y < end {
            return (i, y - start);
        }
        start = end;
    }
    (
        heights.len().saturating_sub(1),
        heights.last().copied().unwrap_or(0),
    )
}
