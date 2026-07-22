use std::path::PathBuf;

use crate::workspace::action::WorkspaceAction;

use super::model::{
    AgentKind, Task, TaskAttachment, TaskId, TaskPriority, TaskStatus, WorkspaceId,
};
use super::panel::{
    TaskSessionRow, partition_task_linked_sessions, task_actions_for, task_presentation,
    tasks_for_selected_workspace,
};

fn task(id: &str, workspace_id: &WorkspaceId, priority: TaskPriority, status: TaskStatus) -> Task {
    Task {
        id: TaskId::new(id),
        workspace_id: workspace_id.clone(),
        workspace: None,
        title: format!("Task {id}"),
        description: String::new(),
        priority,
        agent_kind: AgentKind::Codex,
        attachments: Vec::<TaskAttachment>::new(),
        status,
        terminal_pane_id: None,
        attention_reason: None,
        created_at_ms: 0,
        updated_at_ms: 0,
    }
}

#[test]
fn task_rows_only_list_tasks_for_the_selected_workspace() {
    let github_workspace = WorkspaceId::new("github-project");
    let active_project_workspace = WorkspaceId::new("active-project");
    let tasks = vec![
        task(
            "github-task",
            &github_workspace,
            TaskPriority::High,
            TaskStatus::Pending,
        ),
        task(
            "active-project-task",
            &active_project_workspace,
            TaskPriority::Low,
            TaskStatus::Done,
        ),
    ];

    let selected = tasks_for_selected_workspace(&tasks, Some(&github_workspace));

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, TaskId::new("github-task"));
}

#[test]
fn task_priority_and_status_presentation_are_deterministic() {
    assert_eq!(
        task_presentation(TaskPriority::High, TaskStatus::AttentionRequired),
        task_presentation(TaskPriority::High, TaskStatus::AttentionRequired),
    );
    assert_eq!(
        task_presentation(TaskPriority::High, TaskStatus::AttentionRequired).priority_label,
        "Alta",
    );
    assert_eq!(
        task_presentation(TaskPriority::High, TaskStatus::AttentionRequired).status_label,
        "Requiere atención",
    );
}

#[test]
fn session_partition_separates_explicitly_linked_tasks_without_changing_ordinary_rows() {
    let ordinary_path = PathBuf::from("ordinary-session");
    let linked_path = PathBuf::from("linked-session");
    let rows = vec![
        TaskSessionRow::ordinary(ordinary_path.clone()),
        TaskSessionRow::linked(linked_path.clone(), TaskId::new("linked-task")),
    ];

    let partition = partition_task_linked_sessions(rows);

    assert_eq!(partition.ordinary, vec![ordinary_path]);
    assert_eq!(partition.linked.len(), 1);
    assert_eq!(partition.linked[0].session, linked_path);
    assert_eq!(partition.linked[0].task_id, TaskId::new("linked-task"));
}

#[test]
fn task_panel_selection_only_selects_and_action_mapping_is_deterministic() {
    let task_id = TaskId::new("task-42");
    let actions = task_actions_for(task_id.clone());

    assert!(matches!(actions.select, WorkspaceAction::SelectTask(id) if id == task_id));
    assert!(matches!(
        actions.launch_codex,
        WorkspaceAction::LaunchTask { task_id: id, agent: AgentKind::Codex } if id == task_id
    ));
    assert!(matches!(
        actions.launch_claude_code,
        WorkspaceAction::LaunchTask { task_id: id, agent: AgentKind::ClaudeCode } if id == task_id
    ));
    assert!(matches!(
        actions.open_linked_session,
        WorkspaceAction::OpenLinkedTaskSession(id) if id == task_id
    ));
    assert!(matches!(
        actions.mark_done,
        WorkspaceAction::MarkTaskDone(id) if id == task_id
    ));
}
