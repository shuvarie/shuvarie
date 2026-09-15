use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use shuvarie_core::Role;
use shuvarie_core::session::TreeNodeTool;
use termina::event::{KeyCode, KeyEvent};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme;
use crate::tui::add_provider::centered_rect;
use crate::tui::list::scroll_offset_for;

pub enum TreeMessage {
    Next,
    Prev,
    Fork,
    ToggleSummarize,
    Delete,
    CancelDelete,
    Close,
}

pub enum TreeEffect {
    /// Fork at the selected row: turn rows (prompts, replies, and the tool
    /// rows that resolve to their reply) fork before themselves, recalling
    /// the node's content into the input; summary rows walk to themselves.
    Fork {
        node: u64,
        summarize: bool,
    },
    DeleteBranch {
        node: u64,
    },
    Close,
}

impl std::fmt::Debug for TreeEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TreeEffect::Fork { node, summarize } => {
                write!(f, "Fork {{ node: {node}, summarize: {summarize} }}")
            }
            TreeEffect::DeleteBranch { node } => write!(f, "DeleteBranch {{ node: {node} }}"),
            TreeEffect::Close => write!(f, "Close"),
        }
    }
}

/// One flattened display row of the depth-first tree walk: a message node or
/// (under assistant nodes) one of their tool calls. `fork_node` is the node a
/// fork at this row targets (tool rows fork at the reply they hang from).
struct TreeRow {
    node_id: u64,
    fork_node: u64,
    guide: String,
    glyph: &'static str,
    text: String,
    role: Role,
    on_path: bool,
    is_tip: bool,
    summary: bool,
    deletable: bool,
}

pub struct TreePopup {
    pub open: bool,
    rows: Vec<TreeRow>,
    pub selected: usize,
    pub offset: usize,
    pub summarize: bool,
    pub confirm_delete: bool,
}

impl TreePopup {
    pub fn new() -> Self {
        Self {
            open: false,
            rows: Vec::new(),
            selected: 0,
            offset: 0,
            summarize: false,
            confirm_delete: false,
        }
    }

    pub fn open(&mut self, session: &shuvarie_core::Session) {
        self.open = true;
        self.rows = build_rows(session);
        self.selected = self
            .rows
            .iter()
            .rposition(|row| row.on_path)
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
        self.offset = 0;
        self.summarize = false;
        self.confirm_delete = false;
        self.recompute_offset();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.confirm_delete = false;
    }

    pub fn set_rows(&mut self, session: &shuvarie_core::Session) {
        let keep = self.rows.get(self.selected).map(|row| row.node_id);
        self.rows = build_rows(session);
        self.selected = keep
            .and_then(|id| self.rows.iter().position(|row| row.node_id == id))
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
        self.recompute_offset();
    }

    fn next(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1).min(self.rows.len() - 1);
            self.recompute_offset();
        }
    }

    fn prev(&mut self) {
        if !self.rows.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    fn recompute_offset(&mut self) {
        self.offset = scroll_offset_for(self.selected, self.offset, 0, self.rows.len());
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<TreeMessage> {
        if self.confirm_delete {
            return match key.code {
                KeyCode::Enter => Some(TreeMessage::Delete),
                KeyCode::Escape => Some(TreeMessage::CancelDelete),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(TreeMessage::Close),
            KeyCode::Down | KeyCode::Char('j') => Some(TreeMessage::Next),
            KeyCode::Up | KeyCode::Char('k') => Some(TreeMessage::Prev),
            KeyCode::Enter => Some(TreeMessage::Fork),
            KeyCode::Char('s') => Some(TreeMessage::ToggleSummarize),
            KeyCode::Char('d') => Some(TreeMessage::Delete),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: TreeMessage) -> Option<TreeEffect> {
        if !self.open {
            return None;
        }
        match msg {
            TreeMessage::Close => {
                self.close();
                Some(TreeEffect::Close)
            }
            TreeMessage::Next => {
                self.next();
                None
            }
            TreeMessage::Prev => {
                self.prev();
                None
            }
            TreeMessage::Fork => {
                let row = self.rows.get(self.selected)?;
                Some(TreeEffect::Fork {
                    node: row.fork_node,
                    summarize: self.summarize,
                })
            }
            TreeMessage::ToggleSummarize => {
                self.summarize = !self.summarize;
                None
            }
            TreeMessage::Delete => {
                if self.confirm_delete {
                    let row = self.rows.get(self.selected)?;
                    let node = row.node_id;
                    self.confirm_delete = false;
                    Some(TreeEffect::DeleteBranch { node })
                } else {
                    self.confirm_delete = self
                        .rows
                        .get(self.selected)
                        .is_some_and(|row| row.deletable);
                    None
                }
            }
            TreeMessage::CancelDelete => {
                self.confirm_delete = false;
                None
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(72, 46, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Session tree");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new("empty conversation".to_string()).fg(theme::TEXT_MUTED),
                list_area,
            );
        } else {
            let offset = scroll_offset_for(
                self.selected,
                self.offset,
                list_area.height as usize,
                self.rows.len(),
            );
            let items: Vec<ListItem> = self
                .rows
                .iter()
                .enumerate()
                .skip(offset)
                .take(list_area.height as usize)
                .map(|(idx, row)| self.render_row(row, idx == self.selected))
                .collect();
            frame.render_widget(List::new(items), list_area);
        }

        let hint = if self.confirm_delete {
            theme::help_line(&[("Enter", "delete branch"), ("Esc", "cancel")])
        } else {
            let summarize_label: &str = if self.summarize {
                "summarize: on"
            } else {
                "summarize: off"
            };
            theme::help_line(&[
                ("↑↓", "walk"),
                ("Enter", "fork here"),
                ("s", summarize_label),
                ("d", "delete branch"),
                ("Esc", "close"),
            ])
        };
        frame.render_widget(Paragraph::new(hint).fg(theme::TEXT_MUTED), hint_area);
    }

    fn render_row(&self, row: &TreeRow, is_selected: bool) -> ListItem<'static> {
        let prefix: &str = if is_selected { "▶ " } else { "  " };
        let text = elide(&row.text, 46);
        let (glyph_color, text_color) = match row.role {
            Role::User => (theme::ACCENT, theme::TEXT),
            Role::Assistant if row.summary => (theme::WARNING, theme::WARNING),
            _ if !row.on_path => (theme::TEXT_MUTED, theme::TEXT_DIM),
            _ => (theme::TEXT, theme::TEXT),
        };
        let mut spans = vec![
            Span::raw(prefix).fg(theme::ACCENT),
            Span::raw(row.guide.clone()).fg(theme::TEXT_MUTED),
            Span::raw(row.glyph).fg(glyph_color),
            Span::raw(" "),
            Span::raw(text).fg(text_color),
        ];
        if row.is_tip {
            spans.push(Span::raw(" ●").fg(theme::ACCENT));
        }
        if row.summary {
            spans.push(Span::raw(" [summarized]").fg(theme::WARNING));
        }
        ListItem::new(Line::from(spans)).style(if is_selected {
            ratatui::style::Style::new().bg(theme::ACCENT_BG)
        } else {
            ratatui::style::Style::new()
        })
    }
}

/// Flatten a session tree into depth-first display rows: message nodes in
/// pre-order (siblings by seq) with tool-call rows under their assistant.
fn build_rows(session: &shuvarie_core::Session) -> Vec<TreeRow> {
    let nodes = &session.nodes;
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len() + 1];
    let mut roots: Vec<usize> = Vec::new();
    let mut index_of: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        index_of.insert(node.id, i);
    }
    for (i, node) in nodes.iter().enumerate() {
        match node.parent.and_then(|p| index_of.get(&p).copied()) {
            Some(parent) => children[parent].push(i),
            None => roots.push(i),
        }
    }
    let tip = session
        .nodes
        .iter()
        .find(|n| n.on_path && Some(n.id) == session.leaf_id)
        .map(|n| n.id)
        .or_else(|| {
            session
                .nodes
                .iter()
                .filter(|n| n.on_path)
                .map(|n| n.id)
                .next_back()
        });
    let mut rows = Vec::with_capacity(nodes.len() * 2);
    let root_count = roots.len();
    let mut stack: Vec<(usize, usize, String, bool)> = roots
        .into_iter()
        .enumerate()
        .rev()
        .map(|(p, i)| (i, 0, String::new(), p + 1 == root_count))
        .collect();
    // Depth-first pre-order with guide prefixes; siblings keep their seq
    // order, so the stack pushes them in reverse. Tool rows are pushed after
    // the children, so they pop first — the tool calls surface directly
    // under their assistant node.
    while let Some((idx, depth, inherited, is_last)) = stack.pop() {
        let node = &nodes[idx];
        let guide = if depth == 0 {
            inherited.clone()
        } else {
            format!("{inherited}{} ", if is_last { "└─" } else { "├─" })
        };
        // A root row has no connector, so nothing continues below it.
        let cont = if depth == 0 {
            String::new()
        } else {
            format!("{inherited}{}", if is_last { "   " } else { "│  " })
        };
        let siblings = &children[idx];
        for (child_pos, &child) in siblings.iter().enumerate().rev() {
            let child_last = child_pos + 1 == siblings.len();
            stack.push((child, depth + 1, cont.clone(), child_last));
        }
        let (glyph, text, fork_node) = match node.role {
            Role::User => ("▸", first_line(&node.content), node.id),
            Role::Assistant if node.summary => ("≡", first_line(&node.content), node.id),
            Role::Assistant => ("✦", first_line(&node.content), node.id),
            Role::System => ("·", first_line(&node.content), node.id),
        };
        rows.push(TreeRow {
            node_id: node.id,
            fork_node,
            guide,
            glyph,
            text,
            role: node.role,
            on_path: node.on_path,
            is_tip: Some(node.id) == tip,
            summary: node.summary,
            deletable: !node.on_path,
        });
        for tool in node.tools.iter().rev() {
            rows.push(TreeRow {
                node_id: node.id,
                fork_node: node.id,
                guide: cont.clone(),
                glyph: "⚙",
                text: tool_line(tool),
                role: Role::Assistant,
                on_path: node.on_path,
                is_tip: false,
                summary: false,
                deletable: !node.on_path,
            });
        }
    }
    rows
}

fn tool_line(tool: &TreeNodeTool) -> String {
    let name = match &tool.worker {
        Some(worker) => format!("{} ({worker})", tool.name),
        None => tool.name.clone(),
    };
    let status = if tool.killed {
        "⏹"
    } else if tool.ok {
        "✓"
    } else {
        "✗"
    };
    format!("{status} {name}")
}

fn first_line(content: &str) -> String {
    let line = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(empty)");
    collapse(line)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn elide(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if width + w > max_width.saturating_sub(1) {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_core::session::TreeNode;

    fn node(
        id: u64,
        parent: Option<u64>,
        role: Role,
        seq: u64,
        content: &str,
        on_path: bool,
    ) -> TreeNode {
        TreeNode {
            id,
            parent,
            role,
            seq,
            content: content.into(),
            summary: false,
            interrupted: false,
            tools: Vec::new(),
            on_path,
        }
    }

    fn session_with(nodes: Vec<TreeNode>, leaf: u64) -> shuvarie_core::Session {
        let mut session = shuvarie_core::Session::new();
        session.nodes = nodes;
        session.leaf_id = Some(leaf);
        session
    }

    fn popup_for(session: &shuvarie_core::Session) -> TreePopup {
        let mut popup = TreePopup::new();
        popup.open(session);
        popup
    }

    #[test]
    fn rows_are_preordered_with_tool_leaves_under_assistants() {
        let mut forked = node(3, Some(2), Role::User, 2, "fork prompt", false);
        forked.tools.push(TreeNodeTool {
            name: "read_file".into(),
            ok: true,
            killed: false,
            worker: None,
        });
        let session = session_with(
            vec![
                node(1, None, Role::User, 0, "ask", true),
                node(2, Some(1), Role::Assistant, 1, "reply", true),
                forked,
                node(4, Some(2), Role::Assistant, 3, "branch reply", false),
            ],
            2,
        );
        let popup = popup_for(&session);
        let kinds: Vec<&str> = popup.rows.iter().map(|r| r.glyph).collect();
        assert_eq!(
            kinds,
            vec!["▸", "✦", "▸", "⚙", "✦"],
            "depth-first pre-order, tools under their assistant"
        );
        assert_eq!(popup.rows[0].guide, "");
        assert_eq!(popup.rows[1].guide, "└─ ");
        assert_eq!(popup.rows[2].guide, "   ├─ ");
        assert_eq!(popup.rows[3].guide, "   │  ");
        assert_eq!(popup.rows[4].guide, "   └─ ");
        assert!(!popup.rows[0].deletable, "on-path rows are not deletable");
        assert!(popup.rows[4].deletable);
        assert_eq!(popup.rows[3].fork_node, 3, "tool rows fork at their reply");
    }

    #[test]
    fn selecting_starts_on_the_active_path_tail() {
        let session = session_with(
            vec![
                node(1, None, Role::User, 0, "ask", true),
                node(2, Some(1), Role::Assistant, 1, "reply", true),
                node(3, Some(2), Role::User, 2, "other", false),
            ],
            2,
        );
        let popup = popup_for(&session);
        assert_eq!(popup.rows[popup.selected].node_id, 2);
    }

    #[test]
    fn fork_effect_carries_the_selected_node_and_summarize_flag() {
        let session = session_with(
            vec![
                node(1, None, Role::User, 0, "ask", true),
                node(2, Some(1), Role::Assistant, 1, "reply", true),
            ],
            2,
        );
        let mut popup = popup_for(&session);
        popup.selected = 1;
        popup.summarize = true;
        match popup.update(TreeMessage::Fork) {
            Some(TreeEffect::Fork { node, summarize }) => {
                assert_eq!(node, 2);
                assert!(summarize);
            }
            other => panic!("expected fork effect, got {other:?}"),
        }
    }

    #[test]
    fn delete_requires_a_second_press_and_only_off_path_nodes() {
        let session = session_with(
            vec![
                node(1, None, Role::User, 0, "ask", true),
                node(2, Some(1), Role::Assistant, 1, "off", false),
            ],
            1,
        );
        let mut popup = popup_for(&session);
        popup.selected = 0;
        assert!(
            popup.update(TreeMessage::Delete).is_none(),
            "on-path node cannot be armed for deletion"
        );
        popup.selected = 1;
        assert!(
            popup.update(TreeMessage::Delete).is_none(),
            "first press arms"
        );
        assert!(popup.confirm_delete);
        match popup.update(TreeMessage::Delete) {
            Some(TreeEffect::DeleteBranch { node }) => assert_eq!(node, 2),
            other => panic!("expected delete effect, got {other:?}"),
        }
    }
}
