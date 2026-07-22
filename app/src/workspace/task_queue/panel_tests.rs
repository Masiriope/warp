use std::path::PathBuf;

use crate::workspace::action::WorkspaceAction;

use super::TaskLoadError;
use super::model::{
    AgentKind, Task, TaskAttachment, TaskId, TaskPriority, TaskStatus, WorkspaceId,
};
use super::panel::{
    TaskSessionMetadata, TaskSessionRow, partition_task_linked_sessions, task_actions_for,
    task_presentation, task_queue_diagnostics, task_session_group, tasks_for_selected_workspace,
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
fn task_sessions_group_is_absent_when_no_linked_task_is_running() {
    let ordinary_session = PathBuf::from("ordinary-session");
    let sessions = vec![TaskSessionRow::ordinary(ordinary_session.clone())];

    let (ordinary, task_group) = task_session_group(sessions, |_| None);

    assert_eq!(ordinary, vec![ordinary_session]);
    assert!(task_group.is_none());
}

#[test]
fn task_sessions_group_uses_only_explicit_live_task_metadata() {
    let ordinary_session = PathBuf::from("ordinary-session");
    let linked_session = PathBuf::from("linked-session");
    let linked_task_id = TaskId::new("LOCAL-004");
    let stale_task_id = TaskId::new("stale-task");
    let sessions = vec![
        TaskSessionRow::ordinary(ordinary_session.clone()),
        TaskSessionRow::linked(linked_session.clone(), linked_task_id.clone()),
        TaskSessionRow::linked(PathBuf::from("stale-session"), stale_task_id),
    ];

    let (ordinary, task_group) = task_session_group(sessions, |task_id| {
        (task_id == &linked_task_id).then(|| TaskSessionMetadata {
            task_id: task_id.clone(),
            title: "Revisar firmas".to_owned(),
            priority_label: "Alta",
            status_label: "En curso",
            workspace_name: "MGA-Portal".to_owned(),
        })
    });

    assert_eq!(
        ordinary,
        vec![ordinary_session, PathBuf::from("stale-session")]
    );
    let task_group = task_group.expect("a live explicit task should render its own group");
    assert_eq!(task_group.rows.len(), 1);
    assert_eq!(task_group.rows[0].session, linked_session);
    assert_eq!(task_group.rows[0].metadata.title, "Revisar firmas");
    assert_eq!(task_group.rows[0].metadata.priority_label, "Alta");
    assert_eq!(task_group.rows[0].metadata.workspace_name, "MGA-Portal");
}

#[test]
fn task_sessions_group_keeps_multiple_explicit_tasks_separate_from_manual_sessions() {
    let first_task_id = TaskId::new("LOCAL-005");
    let second_task_id = TaskId::new("LOCAL-006");
    let manual_session = PathBuf::from("manually-started-codexauto");
    let sessions = vec![
        TaskSessionRow::linked(PathBuf::from("first-task-terminal"), first_task_id.clone()),
        TaskSessionRow::ordinary(manual_session.clone()),
        TaskSessionRow::linked(
            PathBuf::from("second-task-terminal"),
            second_task_id.clone(),
        ),
    ];

    let (ordinary, task_group) = task_session_group(sessions, |task_id| {
        if task_id == &first_task_id {
            Some(TaskSessionMetadata {
                task_id: task_id.clone(),
                title: "Primera tarea".to_owned(),
                priority_label: "Normal",
                status_label: "En curso",
                workspace_name: "MGA-Portal".to_owned(),
            })
        } else if task_id == &second_task_id {
            Some(TaskSessionMetadata {
                task_id: task_id.clone(),
                title: "Segunda tarea".to_owned(),
                priority_label: "Baja",
                status_label: "En curso",
                workspace_name: "warp".to_owned(),
            })
        } else {
            None
        }
    });

    assert_eq!(ordinary, vec![manual_session]);
    let task_group = task_group.expect("both explicit tasks should remain grouped");
    assert_eq!(task_group.rows.len(), 2);
    assert_eq!(task_group.rows[0].metadata.task_id, first_task_id);
    assert_eq!(task_group.rows[1].metadata.task_id, second_task_id);
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

#[test]
fn task_queue_diagnostics_surface_partial_load_and_store_failures() {
    let diagnostics = task_queue_diagnostics(
        &[TaskLoadError {
            path: PathBuf::from("/private/TaskQueue/broken/task.md"),
            reason: "missing title".to_owned(),
        }],
        Some("task directory cannot be read"),
    );

    assert_eq!(
        diagnostics,
        vec![
            "No se pudo cargar la cola de tareas: task directory cannot be read".to_owned(),
            "No se pudo cargar /private/TaskQueue/broken/task.md: missing title".to_owned(),
        ]
    );
}
