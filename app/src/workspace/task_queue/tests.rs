use std::fs;

use super::{
    AgentKind, NewTaskInput, Task, TaskId, TaskPriority, TaskQueueModel, TaskStatus, WorkspaceRoot,
    WorkspaceSource, default_sources,
};

#[test]
fn discovers_direct_child_workspaces_in_their_source_groups() {
    let home = tempfile::tempdir().expect("home directory should be created");
    let github = home.path().join("Documents/GitHub");
    let active_projects = home.path().join("Desktop/01_Proyectos_Activos");

    fs::create_dir_all(github.join("MGA-Portal/.git")).expect("github workspace should exist");
    fs::create_dir_all(active_projects.join("warp/.git"))
        .expect("active project workspace should exist");
    fs::write(github.join("README.md"), "not a workspace").expect("file should be created");

    let sources = default_sources(home.path());
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources,
        vec![
            WorkspaceRoot::new(WorkspaceSource::Github, github),
            WorkspaceRoot::new(WorkspaceSource::ActiveProjects, active_projects),
        ]
    );

    let mut model = TaskQueueModel::new();
    model.discover(sources.clone());
    assert_eq!(model.sources(), sources);

    let github_workspaces = model.workspaces_for_source(WorkspaceSource::Github);
    assert_eq!(github_workspaces.len(), 1);
    assert_eq!(github_workspaces[0].display_name, "MGA-Portal");

    let active_workspaces = model.workspaces_for_source(WorkspaceSource::ActiveProjects);
    assert_eq!(active_workspaces.len(), 1);
    assert_eq!(active_workspaces[0].display_name, "warp");
}

#[test]
fn task_requires_explicit_review_before_completion() {
    let workspace_id = super::WorkspaceId::new("workspace-1");
    let mut task = Task::new(
        TaskId::new("task-1"),
        NewTaskInput::new(workspace_id, "Implement task queue")
            .with_priority(TaskPriority::High)
            .with_agent_kind(AgentKind::Codex),
    );

    assert_eq!(task.status, TaskStatus::Pending);
    task.mark_launched("terminal-pane-1")
        .expect("pending task should launch");
    assert_eq!(task.status, TaskStatus::InProgress);
    task.mark_command_finished(true)
        .expect("launched task should record a successful command");
    assert_eq!(task.status, TaskStatus::ReviewRequired);
    task.mark_done()
        .expect("review-required task should be explicitly completed");
    assert_eq!(task.status, TaskStatus::Done);
}

#[test]
fn discovery_sorts_case_insensitively_and_ids_include_the_path() {
    let roots = tempfile::tempdir().expect("workspace roots should be created");
    let github = roots.path().join("github");
    let active_projects = roots.path().join("active");
    fs::create_dir_all(github.join("zebra")).expect("workspace should be created");
    fs::create_dir_all(github.join("Alpha")).expect("workspace should be created");
    fs::create_dir_all(active_projects.join("alpha")).expect("workspace should be created");

    let mut model = TaskQueueModel::new();
    model.discover(vec![
        WorkspaceRoot::new(WorkspaceSource::Github, github),
        WorkspaceRoot::new(WorkspaceSource::ActiveProjects, active_projects),
    ]);

    let github_workspaces = model.workspaces_for_source(WorkspaceSource::Github);
    assert_eq!(github_workspaces[0].display_name, "Alpha");
    assert_eq!(github_workspaces[1].display_name, "zebra");

    let active_workspace = &model.workspaces_for_source(WorkspaceSource::ActiveProjects)[0];
    assert_ne!(github_workspaces[0].id, active_workspace.id);
}

#[test]
fn unavailable_workspace_roots_remain_visible() {
    let unavailable = tempfile::tempdir()
        .expect("temporary directory should be created")
        .path()
        .join("missing");
    let mut model = TaskQueueModel::new();
    model.discover(vec![WorkspaceRoot::new(
        WorkspaceSource::Github,
        unavailable.clone(),
    )]);

    let workspace = &model.workspaces_for_source(WorkspaceSource::Github)[0];
    assert_eq!(workspace.path, unavailable);
    assert!(!workspace.is_available);
}

#[test]
fn task_queue_selects_discovered_workspaces_in_memory() {
    let root = tempfile::tempdir().expect("workspace root should be created");
    let github = root.path().join("github");
    fs::create_dir_all(github.join("MGA-Portal")).expect("workspace should be created");

    let mut model = TaskQueueModel::new();
    model.discover(vec![WorkspaceRoot::new(WorkspaceSource::Github, github)]);
    let workspace_id = model.workspaces()[0].id.clone();

    model
        .select_workspace(workspace_id.clone())
        .expect("discovered workspace should be selectable");

    assert_eq!(model.selected_workspace_id(), Some(&workspace_id));
}

#[test]
fn invalid_task_transitions_preserve_attention_reason() {
    let mut task = Task::new(
        TaskId::new("task-1"),
        NewTaskInput::new(super::WorkspaceId::new("workspace-1"), "Handle a failure"),
    );
    task.mark_launched("terminal-pane-1")
        .expect("pending task should launch");
    task.mark_command_finished(false)
        .expect("launched task should record a failed command");

    assert_eq!(task.status, TaskStatus::AttentionRequired);
    let reason = task.attention_reason.clone();
    assert!(task.mark_done().is_err());
    assert_eq!(task.status, TaskStatus::AttentionRequired);
    assert_eq!(task.attention_reason, reason);
}

#[test]
fn task_queue_keeps_tasks_and_selection_in_memory() {
    let workspace_id = super::WorkspaceId::new("workspace-1");
    let attachment = super::TaskAttachment::new("notes.md");
    let mut task = Task::new(
        TaskId::new("task-1"),
        NewTaskInput::new(workspace_id.clone(), "Review task queue")
            .with_description("Use the local Markdown task queue")
            .with_attachments(vec![attachment]),
    );
    task.mark_launched("terminal-pane-1")
        .expect("pending task should launch");
    task.mark_attention_required("Review the command output")
        .expect("in-progress task should be able to require attention");

    let task_id = task.id.clone();
    let mut model = TaskQueueModel::new();
    model.insert_task(task);

    assert_eq!(
        model
            .task(&task_id)
            .expect("task should be stored")
            .description,
        "Use the local Markdown task queue"
    );
    assert_eq!(model.tasks_for_workspace(&workspace_id).len(), 1);
    model
        .select_task(task_id.clone())
        .expect("stored task should be selectable");
    assert_eq!(model.selected_task_id(), Some(&task_id));
    assert!(model.select_task(TaskId::new("missing-task")).is_err());
}

#[test]
fn agent_kinds_map_to_the_expected_wrappers() {
    assert_eq!(AgentKind::Codex.wrapper_name(), "codexauto");
    assert_eq!(AgentKind::ClaudeCode.wrapper_name(), "claudeauto");
}
