use super::*;
use crate::api::schema::{Method, PaneDirection, PaneTarget};

const CTRL_H: &[u8] = b"\x1b[104;5u";
const CTRL_L: &[u8] = b"\x1b[108;5u";

fn pane(pane_id: &str, x: u16, y: u16, width: u16, height: u16) -> PaneSurfacePane {
    let rect = SurfaceRect {
        x,
        y,
        width,
        height,
    };
    PaneSurfacePane {
        pane_id: pane_id.into(),
        rect,
        inner_rect: rect,
        focused: false,
        ..surface().panes[0].clone()
    }
}

/// pane_1 fills the left column; pane_2 over pane_3 fill the right column.
fn sidebar_state(focused: &str) -> ClientShellState {
    let mut projected = snapshot();
    projected.focused_pane_id = Some(focused.into());
    for pane_id in ["pane_2", "pane_3"] {
        let mut pane = projected.panes[0].clone();
        pane.pane_id = pane_id.into();
        projected.panes.push(pane);
    }
    for (pane_id, status) in [
        ("pane_2", AgentStatus::Working),
        ("pane_3", AgentStatus::Idle),
    ] {
        projected.agents.push(ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: None,
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        });
    }
    let mut frame = surface();
    frame.panes = vec![
        pane("pane_1", 0, 0, 4, 2),
        pane("pane_2", 4, 0, 4, 1),
        pane("pane_3", 4, 1, 4, 1),
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(frame);
    state.compose(106, 24).unwrap();
    state
}

fn focused_panes(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::PaneFocus(PaneTarget { pane_id }) => Some(pane_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn edge(state: &mut ClientShellState, direction: PaneDirection) -> ClientShellInput {
    state.receive_focus_edge(&ClientEndpointId::Local, direction)
}

#[test]
fn horizontal_edge_enters_visible_sidebar() {
    for direction in [PaneDirection::Left, PaneDirection::Right] {
        let mut state = sidebar_state("pane_1");
        let outcome = edge(&mut state, direction);
        assert!(outcome.actions.is_empty());
        assert_eq!(state.mode, ClientShellMode::Navigate);
        assert_eq!(state.sidebar_section, Some(SidebarSection::Spaces));
        assert_eq!(
            state.navigate_workspace_id,
            state.navigation_target(&ClientEndpointId::Local, "ws_1")
        );
    }
}

#[test]
fn vertical_edge_wraps_within_the_column() {
    let mut state = sidebar_state("pane_3");
    assert_eq!(
        focused_panes(&edge(&mut state, PaneDirection::Down)),
        ["pane_2"]
    );
    assert_eq!(state.mode, ClientShellMode::Terminal);

    let mut state = sidebar_state("pane_2");
    assert_eq!(
        focused_panes(&edge(&mut state, PaneDirection::Up)),
        ["pane_3"]
    );
}

#[test]
fn collapsed_sidebar_wraps_horizontally() {
    let mut state = sidebar_state("pane_2");
    state.sidebar_collapsed = true;
    assert_eq!(
        focused_panes(&edge(&mut state, PaneDirection::Right)),
        ["pane_1"]
    );
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn edge_is_ignored_outside_terminal_mode() {
    let mut state = sidebar_state("pane_3");
    state.mode = ClientShellMode::Resize;
    let outcome = edge(&mut state, PaneDirection::Down);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Resize);
}

#[test]
fn ctrl_h_and_l_leave_the_sidebar_like_pane_edges() {
    let mut state = sidebar_state("pane_1");
    edge(&mut state, PaneDirection::Left);
    // ctrl+l returns to the leftmost pane, already focused: no request, but out.
    assert!(focused_panes(&state.handle_input_bytes(CTRL_L)).is_empty());
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert_eq!(state.sidebar_section, None);

    edge(&mut state, PaneDirection::Left);
    assert_eq!(focused_panes(&state.handle_input_bytes(CTRL_H)), ["pane_2"]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn tab_switches_to_agents_and_enter_focuses_the_agent() {
    let mut state = sidebar_state("pane_1");
    edge(&mut state, PaneDirection::Left);
    state.handle_input_bytes(b"\t");
    assert_eq!(state.sidebar_section, Some(SidebarSection::Agents));
    assert_eq!(state.sidebar_agent_pane_id.as_deref(), Some("pane_2"));
    // j wraps past the last agent; k wraps back.
    state.handle_input_bytes(b"j");
    state.handle_input_bytes(b"j");
    assert_eq!(state.sidebar_agent_pane_id.as_deref(), Some("pane_2"));
    state.handle_input_bytes(b"k");
    assert_eq!(state.sidebar_agent_pane_id.as_deref(), Some("pane_3"));
    assert_eq!(focused_panes(&state.handle_input_bytes(b"\r")), ["pane_3"]);
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn h_and_l_fold_worktree_groups_like_a_file_tree() {
    let mut state = sidebar_state("pane_1");
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let template = projected.workspaces[0].clone();
    projected.workspaces = (1..=2)
        .map(|number| {
            let mut workspace = template.clone();
            workspace.workspace_id = format!("ws_{number}");
            workspace.number = number;
            workspace.focused = number == 1;
            workspace.worktree = Some(ClientShellWorktree {
                key: "repo".into(),
                label: "repo".into(),
                is_linked_worktree: number == 2,
            });
            workspace
        })
        .collect();
    state.set_snapshot(Box::new(projected));
    edge(&mut state, PaneDirection::Left);

    // On the linked child, h folds the group and jumps to the parent.
    state.handle_input_bytes(b"j");
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_2")
    );
    state.handle_input_bytes(b"h");
    assert!(state.group_is_collapsed(&ClientEndpointId::Local, "repo"));
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_1")
    );

    // l unfolds first, and only opens the workspace once nothing is folded.
    state.handle_input_bytes(b"l");
    assert!(!state.group_is_collapsed(&ClientEndpointId::Local, "repo"));
    assert_eq!(state.mode, ClientShellMode::Navigate);
    state.handle_input_bytes(b"l");
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn legacy_ctrl_j_and_k_move_like_j_and_k() {
    let mut state = sidebar_state("pane_1");
    edge(&mut state, PaneDirection::Left);
    state.handle_input_bytes(b"\t");
    state.handle_input_bytes(&[0x0a]);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.sidebar_agent_pane_id.as_deref(), Some("pane_3"));
    state.handle_input_bytes(&[0x0b]);
    assert_eq!(state.sidebar_agent_pane_id.as_deref(), Some("pane_2"));
}

#[test]
fn workspace_picker_keeps_its_own_keys() {
    let mut state = sidebar_state("pane_1");
    edge(&mut state, PaneDirection::Left);
    state.handle_input_bytes(b"\x1b");
    state.handle_input_bytes(&[0x02]);
    state.handle_input_bytes(b"w");
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.sidebar_section, None);
}

fn focused_workspaces(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                Method::WorkspaceFocus(target) => Some(target.workspace_id.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn two_space_state() -> ClientShellState {
    let mut state = sidebar_state("pane_1");
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let mut second = projected.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.focused = false;
    projected.workspaces.push(second);
    state.set_snapshot(Box::new(projected));
    state
}

#[test]
fn moving_the_cursor_previews_and_esc_restores() {
    let mut state = two_space_state();
    edge(&mut state, PaneDirection::Left);
    assert_eq!(
        focused_workspaces(&state.handle_input_bytes(b"j")),
        ["ws_2"]
    );
    // Back to the start while that preview is still in flight: still sent.
    assert_eq!(
        focused_workspaces(&state.handle_input_bytes(b"k")),
        ["ws_1"]
    );
    assert_eq!(
        focused_panes(&state.handle_input_bytes(b"\x1b")),
        ["pane_1"]
    );
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn esc_without_a_preview_changes_nothing() {
    let mut state = two_space_state();
    edge(&mut state, PaneDirection::Left);
    assert!(state.handle_input_bytes(b"\x1b").actions.is_empty());
}

#[test]
fn agents_preview_and_enter_keeps_it() {
    let mut state = sidebar_state("pane_1");
    edge(&mut state, PaneDirection::Left);
    assert_eq!(focused_panes(&state.handle_input_bytes(b"\t")), ["pane_2"]);
    assert_eq!(focused_panes(&state.handle_input_bytes(b"j")), ["pane_3"]);
    state.handle_input_bytes(b"\r");
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert_eq!(state.sidebar_restore_pane_id, None);
}

#[test]
fn ctrl_l_keeps_an_unsettled_space_preview() {
    let mut state = two_space_state();
    edge(&mut state, PaneDirection::Left);
    state.handle_input_bytes(b"j");
    // The ws_2 preview has not landed, so no pane from ws_1 may be focused.
    assert!(focused_panes(&state.handle_input_bytes(CTRL_H)).is_empty());
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn a_landed_preview_keeps_the_sidebar_open() {
    let mut state = two_space_state();
    edge(&mut state, PaneDirection::Left);
    state.handle_input_bytes(b"j");
    let mut projected = state.snapshot.as_deref().unwrap().clone();
    projected.revision += 1;
    projected.focused_workspace_id = Some("ws_2".into());
    for workspace in &mut projected.workspaces {
        workspace.focused = workspace.workspace_id == "ws_2";
    }
    state.set_snapshot(Box::new(projected));
    state.compose(106, 24);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(state.sidebar_section, Some(SidebarSection::Spaces));
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_2")
    );
    // Now settled: ctrl+l steps into ws_2's leftmost pane.
    state.handle_input_bytes(CTRL_L);
    assert_eq!(state.mode, ClientShellMode::Terminal);
}
