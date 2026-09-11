use std::cell::RefCell;
use std::rc::Rc;

use ratatui::prelude::*;
use shuvarie_highlight::render_pass;

use super::segment::{BodyChunk, BodySource, CHUNK_ROWS, wrapped_line_count};

/// Lines per committed run: immutable `Rc` bundles shared with already-built
/// segments, so an append allocates only the newly stable region instead of
/// cloning the whole rendered history.
const RUN_LINES: usize = 256;

/// Cached markdown rendering for a chat block: rendered lines plus per-line
/// wrapped-row counts, kept valid across appends by committing at the last
/// safe top-level markdown boundary (`render_pass`) and
/// re-rendering only the open region after it. Static content (user prompts,
/// system notes) never appends; the render happens once on first view.
/// Rendering is width-independent; only the wrapped-row counts depend on the
/// width, so a resize re-counts without re-rendering.
pub struct MdCache {
    content: String,
    rev: u64,
    render: RefCell<Option<Render>>,
}

#[derive(Clone)]
struct LineRun {
    lines: Rc<[Line<'static>]>,
    counts: Rc<[u32]>,
}

struct Render {
    width: u16,
    rev: u64,
    committed: Vec<LineRun>,
    committed_bytes: usize,
    committed_lines: u32,
    committed_width: u32,
    /// The open region after the last commit: re-rendered on every append,
    /// shared with the most recently built chunk list.
    pending: Rc<[Line<'static>]>,
    pending_counts: Rc<[u32]>,
    pending_width: u32,
}

impl MdCache {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            rev: 0,
            render: RefCell::new(None),
        }
    }

    /// Stream a chunk into the content. The render catches up lazily on the
    /// next view, batching every append between frames into one tail render.
    pub fn append(&mut self, chunk: &str) {
        self.content.push_str(chunk);
        self.rev += 1;
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// Body chunks for the rendered content at `width`: committed runs as
    /// sliced chunks over precomputed counts, the open tail after them.
    /// Trailing blank lines are dropped like a one-shot render's finish.
    pub fn chunks(&self, width: u16) -> Option<Vec<BodyChunk>> {
        let width = width.max(1);
        self.ensure(width);
        let render = self.render.borrow();
        let render = render.as_ref()?;
        let end = render.display_end();
        if end == 0 {
            return None;
        }
        let mut chunks = Vec::with_capacity(render.committed.len() + 1);
        let mut covered = 0u32;
        for run in &render.committed {
            if covered >= end {
                break;
            }
            let run_len = run.lines.len() as u32;
            let take = run_len.min(end - covered);
            let counts = if take == run_len {
                run.counts.clone()
            } else {
                // display-trimmed final run
                run.counts[..take as usize].to_vec().into()
            };
            chunks.push(BodyChunk::counted(
                BodySource::Lines {
                    lines: run.lines.clone(),
                },
                0,
                counts,
            ));
            covered += take;
        }
        if covered < end {
            let take = (end - covered) as usize;
            if take <= CHUNK_ROWS as usize {
                chunks.push(BodyChunk::fixed(render.pending[..take].to_vec()));
            } else {
                chunks.push(BodyChunk::counted(
                    BodySource::Lines {
                        lines: render.pending.clone(),
                    },
                    0,
                    render.pending_counts[..take].to_vec().into(),
                ));
            }
        }
        Some(chunks)
    }

    #[cfg(test)]
    pub(crate) fn probe_state(&self) -> (u32, usize, usize) {
        let render = self.render.borrow();
        match render.as_ref() {
            Some(r) => (r.committed_lines, r.pending.len(), r.committed_bytes),
            None => (0, 0, 0),
        }
    }

    /// Estimated row counters `(displayed line count, total display width)`;
    /// `None` until the first view renders the content. Between frames the
    /// counters may lag the latest append by the open region — the estimate
    /// only feeds unmeasured turns, so exactness is not required.
    pub fn est_counters(&self) -> Option<(u32, u32)> {
        let render = self.render.borrow();
        let render = render.as_ref()?;
        Some((
            render.display_end(),
            render.committed_width + render.pending_width,
        ))
    }

    fn ensure(&self, width: u16) {
        let mut render = self.render.borrow_mut();
        match render.as_mut() {
            None => *render = Some(self.full_render(width)),
            Some(r) if r.rev == self.rev && r.width == width => {}
            Some(r) if r.width == width => self.catch_up(r),
            Some(_) => *render = Some(self.full_render(width)),
        }
    }

    fn full_render(&self, width: u16) -> Render {
        let pass = render_pass(&self.content);
        let (stable, committed_bytes) = pass
            .boundaries
            .last()
            .map_or((0usize, 0usize), |&(byte, lines)| (lines, byte));
        let counts: Vec<u32> = pass
            .lines
            .iter()
            .map(|line| wrapped_line_count(line, width, true))
            .collect();
        let widths: Vec<u32> = pass.lines.iter().map(|line| line_width(line)).collect();
        let committed_width: u32 = widths[..stable].iter().sum();
        let pending_width: u32 = widths[stable..].iter().sum();
        let committed = bundle_runs(&pass.lines[..stable], &counts[..stable]);
        Render {
            width,
            rev: self.rev,
            committed,
            committed_bytes,
            committed_lines: stable as u32,
            committed_width,
            pending: pass.lines[stable..].to_vec().into(),
            pending_counts: counts[stable..].to_vec().into(),
            pending_width,
        }
    }

    /// Re-render the open region after the last commit and advance the
    /// boundary: the newly stable lines bundle into runs, the rest stays
    /// pending until the next append re-renders it.
    fn catch_up(&self, render: &mut Render) {
        let tail = &self.content[render.committed_bytes..];
        let pass = render_pass(tail);
        let counts: Vec<u32> = pass
            .lines
            .iter()
            .map(|line| wrapped_line_count(line, render.width, true))
            .collect();
        let widths: Vec<u32> = pass.lines.iter().map(|line| line_width(line)).collect();
        let stable = stable_lines(&pass);
        if let Some(&(byte, _)) = pass.boundaries.last() {
            render
                .committed
                .extend(bundle_runs(&pass.lines[..stable], &counts[..stable]));
            render.committed_bytes += byte;
            render.committed_lines += stable as u32;
            render.committed_width += widths[..stable].iter().sum::<u32>();
            render.pending_width = widths[stable..].iter().sum();
        } else {
            render.pending_width = widths.iter().sum();
        }
        render.pending = pass.lines[stable..].to_vec().into();
        render.pending_counts = counts[stable..].to_vec().into();
        render.rev = self.rev;
    }
}

fn bundle_runs(lines: &[Line<'static>], counts: &[u32]) -> Vec<LineRun> {
    let mut runs = Vec::new();
    let mut at = 0usize;
    while at < lines.len() {
        let n = (lines.len() - at).min(RUN_LINES);
        runs.push(LineRun {
            lines: lines[at..at + n].to_vec().into(),
            counts: counts[at..at + n].to_vec().into(),
        });
        at += n;
    }
    runs
}

/// The last pass boundary's line count, if any.
fn stable_lines(pass: &shuvarie_highlight::MdPass) -> usize {
    pass.boundaries.last().map_or(0, |&(_, lines)| lines)
}

fn line_width(line: &Line<'_>) -> u32 {
    line.width() as u32
}

fn is_blank_line(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.is_empty())
}

impl Render {
    fn total_lines(&self) -> u32 {
        self.committed_lines + self.pending.len() as u32
    }

    fn line_at(&self, index: u32) -> &Line<'static> {
        let i = index as usize;
        let committed = self.committed_lines as usize;
        if i < committed {
            let mut offset = 0usize;
            for run in &self.committed {
                if i < offset + run.lines.len() {
                    return &run.lines[i - offset];
                }
                offset += run.lines.len();
            }
            unreachable!("run lengths sum to committed_lines");
        }
        &self.pending[i - committed]
    }

    /// The displayed line count: trailing blank lines are dropped (a one-shot
    /// render trims the document's trailing blanks; committed lines keep
    /// their interior blank rows).
    fn display_end(&self) -> u32 {
        let mut end = self.total_lines();
        while end > 0 && is_blank_line(self.line_at(end - 1)) {
            end -= 1;
        }
        end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_highlight::render;

    fn lines_of(cache: &MdCache, width: u16) -> Vec<String> {
        cache
            .chunks(width)
            .unwrap_or_default()
            .iter()
            .flat_map(|chunk| match chunk {
                BodyChunk::Fixed(chunk) => chunk.lines.to_vec(),
                BodyChunk::Sliced(chunk) => chunk
                    .rows()
                    .map(|i| chunk.source.row(i))
                    .collect::<Vec<_>>(),
            })
            .map(|line| line.to_string())
            .collect()
    }

    /// Appends the parts with a frame render between each, like the live
    /// turn does (one view per delta), so the committed boundary advances
    /// while the stream is still open.
    fn streamed(parts: &[&str], width: u16) -> Vec<String> {
        let mut cache = MdCache::new("");
        let mut lines = Vec::new();
        for part in parts {
            cache.append(part);
            lines = lines_of(&cache, width);
        }
        lines
    }

    fn one_shot(content: &str) -> Vec<String> {
        render(content).iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn streamed_render_matches_one_shot() {
        let content = "# Title\n\nProse with `code` and *emphasis*.\n\n- bullet one\n- bullet two\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\n> quoted line\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let expected = one_shot(content);
        for width in [20u16, 60, 120] {
            let streamed = streamed(&[content], width);
            assert_eq!(streamed, expected, "width {width}");
        }
    }

    #[test]
    fn char_by_char_streaming_matches_one_shot() {
        let content = "# Header\n\nHello *world* with `code` and a list:\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n\n> quoted\n\nTail line";
        let mut cache = MdCache::new("");
        let mut last = 0usize;
        for (i, _) in content.char_indices().skip(1) {
            cache.append(&content[last..i]);
            last = i;
            let _ = lines_of(&cache, 80);
        }
        cache.append(&content[last..]);
        let lines = lines_of(&cache, 80);
        assert_eq!(lines, one_shot(content));
    }

    #[test]
    fn mid_heading_frame_does_not_split() {
        let mut cache = MdCache::new("");
        cache.append("Intro\n\n# Ti");
        let frozen = lines_of(&cache, 80);
        let (committed, _, bytes) = cache.probe_state();
        cache.append("tle\n\nNext");
        let lines = lines_of(&cache, 80);
        assert_eq!(frozen, one_shot("Intro\n\n# Ti"));
        assert_eq!(lines, one_shot("Intro\n\n# Title\n\nNext"));
        assert_eq!(
            (committed, bytes),
            (2, 6),
            "the unterminated heading line must stay open; only the intro paragraph commits"
        );
    }

    #[test]
    fn terminated_heading_commits_between_frames() {
        let mut cache = MdCache::new("");
        cache.append("Intro\n\n# Title\n");
        let _ = lines_of(&cache, 80);
        let (committed, _, bytes) = cache.probe_state();
        assert_eq!(
            (committed, bytes),
            (4, 15),
            "the newline-terminated heading commits with its trailing blank"
        );
        cache.append("\nNext");
        assert_eq!(lines_of(&cache, 80), one_shot("Intro\n\n# Title\n\nNext"));
    }

    #[test]
    fn mid_rule_frame_does_not_degrade() {
        assert_eq!(streamed(&["---", "text"], 80), one_shot("---text"));
        assert_eq!(streamed(&["***", " done"], 80), one_shot("*** done"));
        assert_eq!(
            streamed(&["before", "\n\n---", " no"], 80),
            one_shot("before\n\n--- no")
        );
    }

    #[test]
    fn terminated_rule_commits_between_frames() {
        assert_eq!(streamed(&["a\n\n---\n", "b"], 80), one_shot("a\n\n---\nb"));
    }

    #[test]
    fn loose_list_stays_uncommitted_until_a_later_block() {
        let mut cache = MdCache::new("");
        cache.append("- a\n\n");
        let _ = lines_of(&cache, 80);
        assert_eq!(
            cache.probe_state().0,
            0,
            "a list boundary never commits on its own"
        );
        cache.append("  indented\n\nplain\n");
        let lines = lines_of(&cache, 80);
        assert_eq!(lines, one_shot("- a\n\n  indented\n\nplain\n"));
        assert_eq!(
            cache.probe_state().0,
            0,
            "still nothing committed: the bare paragraph is unterminated"
        );
        cache.append("\nmore\n");
        let lines = lines_of(&cache, 80);
        let (committed, _, bytes) = cache.probe_state();
        assert_eq!(
            (committed, bytes),
            (6, 23),
            "the paragraph boundary takes the loosened list along"
        );
        assert_eq!(lines, one_shot("- a\n\n  indented\n\nplain\n\nmore\n"));
    }

    #[test]
    fn setext_and_table_separators_stay_uncommitted() {
        let lines = streamed(&["Title", "\n====\n\nafter"], 80);
        assert_eq!(lines, one_shot("Title\n====\n\nafter"));
        let lines = streamed(&["a | b\n", "--- | ---\n", "1 | 2"], 80);
        assert_eq!(lines, one_shot("a | b\n--- | ---\n1 | 2"));
    }

    #[test]
    fn lazy_quote_and_list_continuations_stay_uncommitted() {
        let lines = streamed(&["  > a", "\nb"], 80);
        assert_eq!(lines, one_shot("  > a\nb"));
        let lines = streamed(&["1. a", "\ncont"], 80);
        assert_eq!(lines, one_shot("1. a\ncont"));
    }

    #[test]
    fn unclosed_fence_stays_uncommitted() {
        let lines = streamed(&["```rust\n", "let x = 1;\n", "let y = 2;"], 80);
        assert_eq!(lines, one_shot("```rust\nlet x = 1;\nlet y = 2;"));
    }

    #[test]
    fn closed_fence_commits_without_blank() {
        let lines = streamed(&["```rust\n", "let x = 1;\n", "```\n", "prose"], 80);
        assert_eq!(lines, one_shot("```rust\nlet x = 1;\n```\nprose"));
    }

    #[test]
    fn ordered_numbering_survives_split() {
        let lines = streamed(&["1. one\n\n", "2. two\n\n", "3. three"], 80);
        assert_eq!(lines, one_shot("1. one\n\n2. two\n\n3. three"));
    }

    #[test]
    fn width_change_recounts_without_rebuilding_lines() {
        let cache = MdCache::new("a long line that surely wraps at narrow widths\n\nshort");
        let _ = cache.chunks(120);
        let before = cache.est_counters().unwrap();
        let _ = cache.chunks(20);
        let after = cache.est_counters().unwrap();
        assert_eq!(
            before.0, after.0,
            "rendered line count is width-independent"
        );
        assert_eq!(after.1, before.1, "total width is width-independent");
    }

    #[test]
    fn commits_advance_across_paragraph_breaks() {
        let mut cache = MdCache::new("");
        for i in 0..40 {
            cache.append(&format!("line {i} with prose filler\n"));
            if i % 4 == 3 {
                cache.append("\n");
            }
            let _ = cache.chunks(80);
        }
        let (committed, pending, bytes) = cache.probe_state();
        assert_eq!(
            committed, 50,
            "every paragraph plus its trailing blank must commit as it completes"
        );
        assert_eq!(pending, 0, "the closed document has no open region");
        assert!(
            bytes >= cache.content().len().saturating_sub(1),
            "committed region must cover the document ({bytes}/{})",
            cache.content().len()
        );
    }

    #[test]
    fn open_paragraph_stays_pending_but_grows_slowly() {
        let mut cache = MdCache::new("");
        for i in 0..40 {
            cache.append(&format!("line {i} inside one long paragraph\n"));
            let _ = cache.chunks(80);
        }
        let (_, pending, bytes) = cache.probe_state();
        assert_eq!(
            pending, 41,
            "no blank line: everything stays open (plus the end-of-input flush)"
        );
        assert_eq!(bytes, 0, "nothing committed mid-paragraph");
        let expected = one_shot(
            &(0..40)
                .map(|i| format!("line {i} inside one long paragraph"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert_eq!(lines_of(&cache, 80), expected);
    }
}
