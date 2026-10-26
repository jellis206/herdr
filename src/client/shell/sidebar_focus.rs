//! Keyboard focus inside the sidebar, entered when directional pane focus runs
//! off a horizontal edge. Moves like a Neovim file tree: j/k move, h/l collapse
//! and expand worktree groups, Tab switches spaces/agents, Enter opens, and
//! ctrl+h/ctrl+l leave back into the panes.

use super::*;
use crate::api::schema::PaneDirection;
use crate::layout::NavDirection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SidebarSection {
    Spaces,
    Agents,
}

impl ClientShellState {
    /// Directional focus found no pane in `direction`: enter the sidebar on a
    /// horizontal edge when it is visible, otherwise wrap to the far side.
    pub(crate) fn receive_focus_edge(
        &mut self,
        endpoint_id: &ClientEndpointId,
        direction: PaneDirection,
    ) -> ClientShellInput {
        let mut outcome = ClientShellInput::default();
        if endpoint_id != &self.active_endpoint_id
            || self.mode != ClientShellMode::Terminal
            || self.overlay.is_some()
        {
            return outcome;
        }
        let direction = NavDirection::from(direction);
        let horizontal = matches!(direction, NavDirection::Left | NavDirection::Right);
        if horizontal && self.sidebar_focusable() {
            self.pending_workspace_highlight = None;
            self.mode = ClientShellMode::Navigate;
            self.navigate_workspace_id = self.focused_navigation_target();
            self.reveal_navigation_workspace = true;
            self.sidebar_section = Some(SidebarSection::Spaces);
            outcome.repaint = true;
        } else {
            self.focus_farthest_pane(crate::layout::opposite_direction(direction), &mut outcome);
        }
        outcome
    }

    pub(super) fn sidebar_focus_active(&self) -> bool {
        self.mode == ClientShellMode::Navigate && self.sidebar_section.is_some()
    }

    /// The agent row under the sidebar cursor, for rendering.
    pub(super) fn sidebar_selected_agent(&self) -> Option<&str> {
        (self.sidebar_focus_active() && self.sidebar_section == Some(SidebarSection::Agents))
            .then_some(self.sidebar_agent_pane_id.as_deref())
            .flatten()
    }

    fn sidebar_focusable(&self) -> bool {
        !self.sidebar_collapsed
            && self.last_composed_size.is_some_and(|(cols, rows)| {
                let layout = self.layout(cols, rows);
                layout.sidebar.width > 0 && layout.mobile_header.is_empty()
            })
    }

    /// Focus the last pane reached walking `direction` from the focused pane.
    fn focus_farthest_pane(&mut self, direction: NavDirection, outcome: &mut ClientShellInput) {
        let Some(focused) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_pane_id.clone())
        else {
            return;
        };
        let Some(surface) = self.pane_surface.as_ref() else {
            return;
        };
        let rect = |pane: &crate::protocol::PaneSurfacePane| {
            Rect::new(pane.rect.x, pane.rect.y, pane.rect.width, pane.rect.height)
        };
        let Some(mut current) = surface.panes.iter().find(|pane| pane.pane_id == focused) else {
            return;
        };
        // Each step moves strictly in `direction`; the bound guards degenerate rects.
        for _ in 0..surface.panes.len() {
            let candidates = surface.panes.iter().map(|pane| (pane, rect(pane)));
            match crate::layout::nearest_in_direction(rect(current), direction, candidates) {
                Some(next) => current = next,
                None => break,
            }
        }
        if current.pane_id != focused {
            let pane_id = current.pane_id.clone();
            self.push_endpoint_method(
                crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
                outcome,
            );
        }
    }

    pub(super) fn route_sidebar_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        if self.config.keybinds.matches_prefix(key) {
            self.leave_sidebar(outcome);
            return;
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let ctrl = modifiers == crossterm::event::KeyModifiers::CONTROL;
        if !ctrl && !modifiers.is_empty() {
            return;
        }
        let agents = self.sidebar_section == Some(SidebarSection::Agents);
        match code {
            KeyCode::Esc => self.leave_sidebar(outcome),
            KeyCode::Char('[') if ctrl => self.leave_sidebar(outcome),
            // Like the pane edge that brought us here: ctrl+l goes back to the
            // leftmost pane, ctrl+h wraps around to the rightmost one.
            KeyCode::Char('l') if ctrl => {
                self.leave_sidebar(outcome);
                self.focus_farthest_pane(NavDirection::Left, outcome);
            }
            KeyCode::Char('h') if ctrl => {
                self.leave_sidebar(outcome);
                self.focus_farthest_pane(NavDirection::Right, outcome);
            }
            KeyCode::Char('j') | KeyCode::Down if agents => self.move_sidebar_agent(1),
            KeyCode::Char('k') | KeyCode::Up if agents => self.move_sidebar_agent(-1),
            KeyCode::Char('j') | KeyCode::Down => self.move_navigate_workspace(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_navigate_workspace(-1),
            KeyCode::Tab | KeyCode::BackTab if !ctrl => self.toggle_sidebar_section(),
            KeyCode::Char('h') | KeyCode::Left if !agents => self.collapse_sidebar_group(outcome),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter if !ctrl => {
                if agents {
                    self.open_sidebar_agent(outcome);
                } else if !self.expand_sidebar_group(outcome) {
                    self.accept_navigate_workspace(outcome);
                }
            }
            _ => {}
        }
    }

    fn leave_sidebar(&mut self, outcome: &mut ClientShellInput) {
        self.mode = self.copy_or_terminal_mode();
        self.navigate_workspace_id = None;
        self.sidebar_section = None;
        outcome.repaint = true;
    }

    fn sidebar_agents(&self) -> Vec<String> {
        self.snapshot
            .as_deref()
            .map(|snapshot| {
                super::agent_sidebar::ordered_agent_pane_ids(snapshot, self.config.agent_panel_sort)
            })
            .unwrap_or_default()
    }

    fn toggle_sidebar_section(&mut self) {
        if self.sidebar_section == Some(SidebarSection::Agents) {
            self.sidebar_section = Some(SidebarSection::Spaces);
            return;
        }
        let agents = self.sidebar_agents();
        if agents.is_empty() {
            return;
        }
        let focused = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_pane_id.as_deref());
        let start = agents
            .iter()
            .find(|pane_id| Some(pane_id.as_str()) == focused)
            .unwrap_or(&agents[0])
            .clone();
        self.sidebar_agent_pane_id = Some(start);
        self.sidebar_section = Some(SidebarSection::Agents);
    }

    fn move_sidebar_agent(&mut self, delta: isize) {
        let agents = self.sidebar_agents();
        if agents.is_empty() {
            return;
        }
        let current = self
            .sidebar_agent_pane_id
            .as_ref()
            .and_then(|selected| agents.iter().position(|pane_id| pane_id == selected))
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(agents.len() as isize) as usize;
        self.sidebar_agent_pane_id = Some(agents[next].clone());
    }

    fn open_sidebar_agent(&mut self, outcome: &mut ClientShellInput) {
        let Some(pane_id) = self
            .sidebar_agent_pane_id
            .clone()
            .filter(|pane_id| self.sidebar_agents().contains(pane_id))
        else {
            return;
        };
        self.leave_sidebar(outcome);
        self.push_endpoint_method(
            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
            outcome,
        );
    }

    /// The selected workspace's index in the active snapshot. Group folding only
    /// applies there; other machines' rows just open on l.
    fn selected_local_workspace(&self) -> Option<usize> {
        let target = self.navigate_workspace_id.as_ref()?;
        if target.endpoint_id != self.active_endpoint_id {
            return None;
        }
        self.snapshot
            .as_deref()?
            .workspaces
            .iter()
            .position(|workspace| workspace.workspace_id == target.workspace_id)
    }

    /// nvim-tree `h`: fold the selected group, or fold a child's group and
    /// jump to its parent row.
    fn collapse_sidebar_group(&mut self, outcome: &mut ClientShellInput) {
        let Some(index) = self.selected_local_workspace() else {
            return;
        };
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(worktree) = snapshot.workspaces[index].worktree.as_ref() else {
            return;
        };
        let key = worktree.key.clone();
        let parent = if worktree.is_linked_worktree {
            snapshot.workspaces.iter().find(|workspace| {
                workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|candidate| candidate.key == key && !candidate.is_linked_worktree)
            })
        } else {
            sidebar::parent_group_key(snapshot, index).map(|_| &snapshot.workspaces[index])
        };
        let Some(parent_id) = parent.map(|workspace| workspace.workspace_id.clone()) else {
            return;
        };
        let endpoint_id = self.active_endpoint_id.clone();
        if !self.group_is_collapsed(&endpoint_id, &key) {
            self.toggle_collapsed_group(&endpoint_id, key);
            self.persist_chrome_preferences(outcome);
        }
        self.navigate_workspace_id = self.navigation_target(&endpoint_id, &parent_id);
    }

    /// nvim-tree `l` on a folded group: unfold it. Returns false when there is
    /// nothing to unfold, so `l` opens the workspace instead.
    fn expand_sidebar_group(&mut self, outcome: &mut ClientShellInput) -> bool {
        let Some(index) = self.selected_local_workspace() else {
            return false;
        };
        let Some(key) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| sidebar::parent_group_key(snapshot, index))
        else {
            return false;
        };
        let endpoint_id = self.active_endpoint_id.clone();
        if !self.group_is_collapsed(&endpoint_id, &key) {
            return false;
        }
        self.toggle_collapsed_group(&endpoint_id, key);
        self.persist_chrome_preferences(outcome);
        true
    }
}
