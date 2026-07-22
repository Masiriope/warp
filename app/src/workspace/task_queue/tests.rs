use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use super::model::TaskQueueEvent;
use super::{
    AgentKind, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority, TaskQueueError,
    TaskQueueModel, TaskStatus, TaskStore, TaskWorkspace, WorkspaceId, WorkspaceRoot,
    WorkspaceSource, build_launch_command, default_sources, shell_quote,
};
use warpui::App;

fn stored_workspace(root: &Path) -> TaskWorkspace {
    TaskWorkspace::new(
        WorkspaceId::new("workspace-1"),
        WorkspaceSource::Github,
        root,
        "workspace-1",
    )
}

fn stored_task_input(workspace: TaskWorkspace) -> NewTaskInput {
    NewTaskInput::new(workspace.id.clone(), "Persist task queue")
        .with_workspace(workspace)
        .with_description("## Context\n\nKeep this local.")
        .with_priority(TaskPriority::High)
        .with_agent_kind(AgentKind::ClaudeCode)
}

#[test]
fn creating_a_task_persists_markdown_and_local_attachment_bytes() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let attachment = data.path().join("notes.txt");
    fs::write(&attachment, b"attachment bytes").expect("attachment should be created");
    let store = TaskStore::open(data.path());
    assert_eq!(store.root(), data.path().join("TaskQueue/v1"));

    let task = store
        .create(
            stored_task_input(stored_workspace(workspace.path()))
                .with_attachments(vec![TaskAttachment::new(&attachment)]),
        )
        .expect("task should persist");

    let task_dir = store.task_directory(&task.workspace_id, &task.id);
    let markdown =
        fs::read_to_string(task_dir.join("task.md")).expect("task markdown should exist");
    assert!(markdown.starts_with("---\n"));
    assert!(markdown.contains("status: \"pending\""));
    assert!(markdown.contains("title: \"Persist task queue\""));
    assert!(markdown.contains("priority: \"high\""));
    assert!(markdown.contains("workspace_path:"));
    assert!(markdown.contains("created_at_ms:"));
    assert!(markdown.contains("attachments:\n  - \"attachments/notes.txt\""));
    assert!(markdown.ends_with("## Context\n\nKeep this local."));
    assert_eq!(
        fs::read(task_dir.join("attachments/notes.txt")).expect("attachment should be copied"),
        b"attachment bytes"
    );
    assert_eq!(task.attachments[0].path, Path::new("attachments/notes.txt"));
}

#[test]
fn attachment_write_failure_leaves_no_visible_task_or_list_entry() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());

    let result = store.create(
        stored_task_input(stored_workspace(workspace.path()))
            .with_attachments(vec![TaskAttachment::new(data.path().join("missing.txt"))]),
    );

    assert!(result.is_err());
    assert!(
        store
            .list()
            .expect("task list should load")
            .tasks
            .is_empty()
    );
    assert!(
        !data.path().join("TaskQueue/v1/tasks/workspace-1").exists(),
        "failed writes must not expose a task directory"
    );
}

#[test]
fn unsafe_attachment_destination_names_are_rejected_before_a_task_is_visible() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let attachment = data.path().join("notes.txt");
    fs::write(&attachment, "attachment").expect("attachment should be created");
    let store = TaskStore::open(data.path());

    let result = store.create(
        stored_task_input(stored_workspace(workspace.path())).with_attachments(vec![
            TaskAttachment::new(attachment).with_file_name("../outside.txt"),
        ]),
    );

    assert!(result.is_err());
    assert!(
        store
            .list()
            .expect("task list should load")
            .tasks
            .is_empty()
    );
    assert!(!data.path().join("TaskQueue/v1/tasks").exists());
}

#[test]
fn stored_tasks_round_trip_metadata_context_and_attachment_references() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let attachment = data.path().join("design.md");
    fs::write(&attachment, "design context").expect("attachment should be created");
    let store = TaskStore::open(data.path());
    let created = store
        .create(
            stored_task_input(stored_workspace(workspace.path()))
                .with_attachments(vec![TaskAttachment::new(&attachment)]),
        )
        .expect("task should persist");

    let loaded = store.list().expect("task list should load");

    assert!(loaded.errors.is_empty());
    assert_eq!(loaded.tasks, vec![created]);
}

#[test]
fn front_matter_escapes_newlines_so_task_fields_cannot_inject_headers() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace_directory = tempfile::tempdir().expect("workspace should be created");
    let workspace = TaskWorkspace::new(
        WorkspaceId::new("workspace-1"),
        WorkspaceSource::Github,
        workspace_directory.path(),
        "workspace-1",
    );
    let store = TaskStore::open(data.path());
    let title = "A title\n---\nstatus: done";

    let task = store
        .create(NewTaskInput::new(workspace.id.clone(), title).with_workspace(workspace))
        .expect("task should persist");
    let markdown = fs::read_to_string(
        store
            .task_directory(&task.workspace_id, &task.id)
            .join("task.md"),
    )
    .expect("task markdown should exist");
    let loaded = store.list().expect("task should parse again");

    assert!(markdown.contains("title: \"A title\\n---\\nstatus: done\""));
    assert_eq!(loaded.tasks, vec![task]);
}

#[test]
fn malformed_markdown_reports_an_inspectable_load_error_without_hiding_valid_tasks() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let valid = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("valid task should persist");
    let broken = data
        .path()
        .join("TaskQueue/v1/tasks/workspace-1/broken-task");
    fs::create_dir_all(&broken).expect("broken task directory should be created");
    fs::write(
        broken.join("task.md"),
        "---\nstatus: pending\n---\nmissing fields",
    )
    .expect("broken markdown should be written");

    let loaded = store
        .list()
        .expect("task list should continue after a parse error");

    assert_eq!(loaded.tasks, vec![valid]);
    assert_eq!(loaded.errors.len(), 1);
    assert_eq!(loaded.errors[0].path, broken.join("task.md"));
    assert!(loaded.errors[0].reason.contains("missing"));
}

#[test]
fn malformed_task_ids_parent_paths_and_duplicate_attachments_are_reported_individually() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let valid = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("valid task should persist");
    let unsafe_id = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("unsafe-id fixture should persist first");
    let duplicate_attachments = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("duplicate-attachment fixture should persist first");
    let noncanonical_attachments = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("noncanonical-attachment fixture should persist first");

    let unsafe_id_path = store
        .task_directory(&unsafe_id.workspace_id, &unsafe_id.id)
        .join("task.md");
    let unsafe_id_markdown = fs::read_to_string(&unsafe_id_path)
        .expect("unsafe-id fixture markdown should be readable")
        .replacen(
            &format!("id: \"{}\"", unsafe_id.id.0),
            "id: \"unsafe/task\"",
            1,
        );
    fs::write(&unsafe_id_path, unsafe_id_markdown).expect("unsafe-id markdown should be written");

    let duplicate_path = store
        .task_directory(
            &duplicate_attachments.workspace_id,
            &duplicate_attachments.id,
        )
        .join("task.md");
    let duplicate_markdown = fs::read_to_string(&duplicate_path)
        .expect("duplicate fixture markdown should be readable")
        .replacen(
            "attachments:\n",
            "attachments:\n  - \"attachments/duplicate.txt\"\n  - \"attachments/duplicate.txt\"\n",
            1,
        );
    fs::write(&duplicate_path, duplicate_markdown)
        .expect("duplicate attachment markdown should be written");

    let noncanonical_path = store
        .task_directory(
            &noncanonical_attachments.workspace_id,
            &noncanonical_attachments.id,
        )
        .join("task.md");
    let noncanonical_markdown = fs::read_to_string(&noncanonical_path)
        .expect("noncanonical fixture markdown should be readable")
        .replacen(
            "attachments:\n",
            "attachments:\n  - \"attachments/duplicate.txt\"\n  - \"attachments/./duplicate.txt\"\n",
            1,
        );
    fs::write(&noncanonical_path, noncanonical_markdown)
        .expect("noncanonical attachment markdown should be written");

    let mismatched_path = data
        .path()
        .join("TaskQueue/v1/tasks/other-workspace/other-task/task.md");
    fs::create_dir_all(
        mismatched_path
            .parent()
            .expect("task path should have a parent"),
    )
    .expect("mismatched task directory should be created");
    fs::copy(
        store
            .task_directory(&valid.workspace_id, &valid.id)
            .join("task.md"),
        &mismatched_path,
    )
    .expect("valid markdown should be copied to mismatched location");

    let loaded = store
        .list()
        .expect("task list should continue after errors");

    assert_eq!(loaded.tasks, vec![valid]);
    assert_eq!(loaded.errors.len(), 4);
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.reason.contains("unsafe task storage path component"))
    );
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.reason.contains("does not match its parent directory"))
    );
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.reason.contains("duplicate attachment filename"))
    );
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.reason.contains("unsafe attachment filename"))
    );
}

#[test]
fn missing_workspace_path_loads_task_as_attention_required_without_deleting_it() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let created = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let workspace_path = workspace.path().to_path_buf();
    fs::remove_dir_all(&workspace_path).expect("workspace should be removed");

    let loaded = store.list().expect("task list should load");

    assert_eq!(loaded.tasks.len(), 1);
    assert_eq!(loaded.tasks[0].id, created.id);
    assert_eq!(loaded.tasks[0].status, TaskStatus::AttentionRequired);
    assert!(
        loaded.tasks[0]
            .attention_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("workspace_path_missing"))
    );
    assert!(
        store
            .task_directory(&created.workspace_id, &created.id)
            .exists()
    );
}

#[test]
fn direct_create_failure_never_exposes_a_half_written_task() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let missing = data.path().join("unreadable-source.txt");

    assert!(
        store
            .create(
                stored_task_input(stored_workspace(workspace.path()))
                    .with_attachments(vec![TaskAttachment::new(missing)]),
            )
            .is_err()
    );

    let tasks_root = data.path().join("TaskQueue/v1/tasks");
    assert!(
        !tasks_root.exists()
            || fs::read_dir(tasks_root)
                .expect("tasks root should be readable")
                .next()
                .is_none()
    );
}

#[test]
fn model_inserts_a_task_only_after_the_store_persists_it() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let mut model = TaskQueueModel::new();

    let task = model
        .create_with_store(
            &store,
            stored_task_input(stored_workspace(workspace.path())),
        )
        .expect("task should persist before entering the model");

    assert_eq!(model.task(&task.id), Some(&task));
}

#[test]
fn launch_transition_is_persisted_before_the_model_exposes_in_progress() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let created = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let mut model = TaskQueueModel::new();
    model
        .load_from_store(&store)
        .expect("task should load into the queue");

    let launched = model
        .launch_with_store(
            &store,
            &created.id,
            AgentKind::ClaudeCode,
            "terminal-pane-1",
        )
        .expect("launch transition should persist");

    assert_eq!(launched.status, TaskStatus::InProgress);
    assert_eq!(launched.agent_kind, AgentKind::ClaudeCode);
    assert_eq!(
        launched.terminal_pane_id.as_deref(),
        Some("terminal-pane-1")
    );
    assert_eq!(model.task(&created.id), Some(&launched));
    assert_eq!(
        store.list().expect("persisted task should reload").tasks,
        vec![launched]
    );
}

#[test]
fn failed_linked_command_is_persisted_as_attention_required_without_a_session_link() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let created = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let mut model = TaskQueueModel::new();
    model
        .load_from_store(&store)
        .expect("task should load into the queue");
    model
        .launch_with_store(&store, &created.id, AgentKind::Codex, "terminal-pane-1")
        .expect("launch transition should persist");

    let completed = model
        .finish_linked_command_with_store(&store, &created.id, false)
        .expect("failed command transition should persist");

    assert_eq!(completed.status, TaskStatus::AttentionRequired);
    assert_eq!(completed.terminal_pane_id, None);
    assert!(completed.attention_reason.is_some());
    assert_eq!(
        store.list().expect("persisted task should reload").tasks,
        vec![completed]
    );
}

#[test]
fn successful_linked_command_is_persisted_as_review_required_without_a_session_link() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let created = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let mut model = TaskQueueModel::new();
    model
        .load_from_store(&store)
        .expect("task should load into the queue");
    model
        .launch_with_store(&store, &created.id, AgentKind::Codex, "terminal-pane-1")
        .expect("launch transition should persist");

    let completed = model
        .finish_linked_command_with_store(&store, &created.id, true)
        .expect("successful command transition should persist");

    assert_eq!(completed.status, TaskStatus::ReviewRequired);
    assert_eq!(completed.terminal_pane_id, None);
    assert_eq!(
        store.list().expect("persisted task should reload").tasks,
        vec![completed]
    );
}

#[test]
fn failed_launch_persistence_keeps_the_model_task_pending_and_unlinked() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let created = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let mut model = TaskQueueModel::new();
    model
        .load_from_store(&store)
        .expect("task should load into the queue");
    fs::remove_file(store.task_markdown_path(&created.workspace_id, &created.id))
        .expect("task file should be removed to force a persistence error");

    assert!(
        model
            .launch_with_store(&store, &created.id, AgentKind::Codex, "terminal-pane-1")
            .is_err()
    );

    let unchanged = model
        .task(&created.id)
        .expect("model task should remain available");
    assert_eq!(unchanged.status, TaskStatus::Pending);
    assert_eq!(unchanged.terminal_pane_id, None);
}

#[test]
fn model_uses_discovered_workspace_metadata_when_persisting_an_input_by_id() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let sources = tempfile::tempdir().expect("workspace sources should be created");
    let github = sources.path().join("github");
    fs::create_dir_all(github.join("workspace-1")).expect("workspace should be created");
    let mut model =
        TaskQueueModel::from_roots(vec![WorkspaceRoot::new(WorkspaceSource::Github, github)]);
    let workspace_id = model.workspaces()[0].id.clone();
    let store = TaskStore::open(data.path());

    let task = model
        .create_with_store(
            &store,
            NewTaskInput::new(workspace_id, "Use discovered metadata"),
        )
        .expect("discovered workspace should supply durable metadata");

    assert!(task.workspace.is_some());
    assert_eq!(
        store.list().expect("task list should load").tasks,
        vec![task]
    );
}

#[test]
fn model_rejects_an_unavailable_workspace_before_persisting_any_task_data() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let unavailable = data.path().join("unavailable-workspace");
    let mut model = TaskQueueModel::from_roots(vec![WorkspaceRoot::new(
        WorkspaceSource::Github,
        unavailable,
    )]);
    let workspace_id = model.workspaces()[0].id.clone();
    let store = TaskStore::open(data.path());

    let error = model
        .create_with_store(
            &store,
            NewTaskInput::new(workspace_id.clone(), "Do not persist"),
        )
        .expect_err("unavailable workspace should fail before persistence");

    assert!(error.to_string().contains("workspace path is unavailable"));
    assert!(model.tasks_for_workspace(&workspace_id).is_empty());
    assert!(
        store
            .list()
            .expect("task list should load")
            .tasks
            .is_empty()
    );
    assert!(!store.root().join("tasks").exists());
    assert!(!store.root().join("workspaces.json").exists());
}

#[cfg(unix)]
#[test]
fn non_utf8_workspace_paths_are_rejected_before_storage_is_created() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace_name = OsString::from_vec(b"workspace-\x80".to_vec());
    let workspace_path = data.path().join(workspace_name);
    let store = TaskStore::open(data.path());

    let error = store
        .create(
            NewTaskInput::new(WorkspaceId::new("non-utf8-workspace"), "Do not persist")
                .with_workspace(TaskWorkspace::new(
                    WorkspaceId::new("non-utf8-workspace"),
                    WorkspaceSource::Github,
                    workspace_path,
                    "non-utf8 workspace",
                )),
        )
        .expect_err("non-UTF8 workspace paths should fail before staging");

    assert!(
        error
            .to_string()
            .contains("workspace path is not valid UTF-8 for task storage")
    );
    assert!(
        store
            .list()
            .expect("task list should load")
            .tasks
            .is_empty()
    );
    assert!(!store.root().join("tasks").exists());
    assert!(!store.root().join("workspaces.json").exists());
}

#[test]
fn model_can_load_persisted_tasks_without_coupling_storage_to_ui() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let task = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let mut model = TaskQueueModel::new();

    let errors = model
        .load_from_store(&store)
        .expect("persisted task should load into the model");

    assert!(errors.is_empty());
    assert_eq!(model.task(&task.id), Some(&task));
}

#[test]
fn task_queue_creation_notifies_each_window_subscriber() {
    App::test((), |mut app| async move {
        let data = tempfile::tempdir().expect("data directory should be created");
        let workspace = tempfile::tempdir().expect("workspace should be created");
        let store = TaskStore::open(data.path());
        let queue = app.add_model(|_| TaskQueueModel::new());
        let notifications = Arc::new(AtomicUsize::new(0));
        app.update(|ctx| {
            for _ in 0..2 {
                let notifications = notifications.clone();
                ctx.subscribe_to_model(&queue, move |_, event: &TaskQueueEvent, _| {
                    if matches!(event, TaskQueueEvent::Updated) {
                        notifications.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });

        queue.update(&mut app, |queue, ctx| {
            queue
                .create_with_store_and_notify(
                    &store,
                    stored_task_input(stored_workspace(workspace.path())),
                    ctx,
                )
                .expect("task should persist and notify");
        });

        assert_eq!(notifications.load(Ordering::Relaxed), 2);
    });
}

#[test]
fn model_initialization_loads_persisted_tasks_and_reconciles_workspace_metadata() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let store = TaskStore::open(data.path());
    let task = store
        .create(stored_task_input(stored_workspace(workspace.path())))
        .expect("task should persist");
    let malformed = store.root().join("tasks/workspace-1/broken-task");
    fs::create_dir_all(&malformed).expect("malformed task directory should be created");
    fs::write(
        malformed.join("task.md"),
        "---\nstatus: pending\n---\nmissing fields",
    )
    .expect("malformed task should be written");

    let model = TaskQueueModel::from_roots_and_store(Vec::new(), &store);

    assert_eq!(model.task(&task.id), Some(&task));
    assert_eq!(model.load_errors().len(), 1);
    assert_eq!(model.load_errors()[0].path, malformed.join("task.md"));
    assert!(model.store_load_error().is_none());
    assert!(
        model
            .workspaces()
            .iter()
            .any(|workspace| workspace.id == task.workspace_id)
    );
}

#[test]
fn failed_startup_store_load_keeps_the_queue_usable_and_records_the_error() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let store = TaskStore::open(data.path());
    fs::create_dir_all(store.root()).expect("store root should be created");
    fs::write(store.root().join("tasks"), "not a directory")
        .expect("invalid task root should be written");

    let model = TaskQueueModel::from_roots_and_store(Vec::new(), &store);

    assert!(model.workspaces().is_empty());
    assert!(model.load_errors().is_empty());
    assert!(model.store_load_error().is_some());
}

#[test]
fn loaded_tasks_keep_removed_workspaces_selectable_and_inspectable() {
    let data = tempfile::tempdir().expect("data directory should be created");
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let workspace_path = workspace.path().to_path_buf();
    let store = TaskStore::open(data.path());
    let task = store
        .create(stored_task_input(stored_workspace(&workspace_path)))
        .expect("task should persist");
    fs::remove_dir_all(&workspace_path).expect("workspace should be removed");

    let model = TaskQueueModel::from_roots_and_store(Vec::new(), &store);
    let restored_workspace = model
        .workspaces()
        .iter()
        .find(|workspace| workspace.id == task.workspace_id)
        .expect("stored workspace metadata should remain visible");
    let restored_task = model
        .task(&task.id)
        .expect("stored task should not be discarded");

    assert!(!restored_workspace.is_available);
    assert_eq!(restored_task.status, TaskStatus::AttentionRequired);
    assert!(restored_task.attention_reason.is_some());
}

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
fn model_construction_from_roots_discovers_workspaces() {
    let home = tempfile::tempdir().expect("home directory should be created");
    let github = home.path().join("Documents/GitHub");
    let active_projects = home.path().join("Desktop/01_Proyectos_Activos");
    fs::create_dir_all(github.join("MGA-Portal")).expect("github workspace should be created");
    fs::create_dir_all(active_projects.join("warp"))
        .expect("active project workspace should be created");

    let model = TaskQueueModel::from_roots(default_sources(home.path()));

    assert_eq!(model.workspaces().len(), 2);
    assert_eq!(model.initialization_error(), None);
}

#[test]
fn model_without_a_home_remains_usable_and_reports_its_initialization_error() {
    let home = tempfile::tempdir().expect("home directory should be created");
    let github = home.path().join("github");
    fs::create_dir_all(github.join("MGA-Portal")).expect("workspace should be created");

    let mut model = TaskQueueModel::from_home(None);
    assert!(model.workspaces().is_empty());
    assert_eq!(
        model.initialization_error(),
        Some(&TaskQueueError::HomeDirectoryUnavailable)
    );

    model.discover(vec![WorkspaceRoot::new(WorkspaceSource::Github, github)]);
    assert_eq!(model.workspaces().len(), 1);
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
fn pending_tasks_can_require_attention_and_retry_without_losing_the_reason_early() {
    let mut task = Task::new(
        TaskId::new("task-1"),
        NewTaskInput::new(WorkspaceId::new("workspace-1"), "Retry failed launch"),
    );

    task.mark_attention_required("Workspace was unavailable")
        .expect("pending task should be able to report a launch failure");
    assert_eq!(task.status, TaskStatus::AttentionRequired);
    assert_eq!(
        task.attention_reason.as_deref(),
        Some("Workspace was unavailable")
    );
    assert!(task.mark_done().is_err());
    assert!(task.mark_command_finished(true).is_err());
    assert_eq!(task.status, TaskStatus::AttentionRequired);
    assert_eq!(
        task.attention_reason.as_deref(),
        Some("Workspace was unavailable")
    );

    task.mark_launched("terminal-pane-2")
        .expect("attention-required task should be retryable");
    assert_eq!(task.status, TaskStatus::InProgress);
    assert_eq!(task.attention_reason, None);
    task.mark_command_finished(true)
        .expect("retried task should still require review");
    assert_eq!(task.status, TaskStatus::ReviewRequired);
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
fn workspace_ids_preserve_leading_parent_components() {
    assert_ne!(
        WorkspaceId::from_path(Path::new("../../repo")),
        WorkspaceId::from_path(Path::new("repo"))
    );
}

#[cfg(unix)]
#[test]
fn workspace_ids_distinguish_non_utf8_path_bytes() {
    let first = OsString::from_vec(b"/tmp/repo-\x80".to_vec());
    let second = OsString::from_vec(b"/tmp/repo-\x81".to_vec());

    assert_ne!(
        WorkspaceId::from_path(Path::new(&first)),
        WorkspaceId::from_path(Path::new(&second))
    );
}

#[test]
fn workspaces_with_matching_display_names_have_distinct_path_ids() {
    let home = tempfile::tempdir().expect("home directory should be created");
    let github = home.path().join("Documents/GitHub");
    let active_projects = home.path().join("Desktop/01_Proyectos_Activos");
    fs::create_dir_all(github.join("Shared")).expect("github workspace should be created");
    fs::create_dir_all(active_projects.join("Shared"))
        .expect("active project workspace should be created");

    let mut model = TaskQueueModel::new();
    model.discover(default_sources(home.path()));

    let github_workspace = &model.workspaces_for_source(WorkspaceSource::Github)[0];
    let active_workspace = &model.workspaces_for_source(WorkspaceSource::ActiveProjects)[0];
    assert_eq!(github_workspace.display_name, "Shared");
    assert_eq!(active_workspace.display_name, "Shared");
    assert_ne!(github_workspace.id, active_workspace.id);
}

#[test]
fn discovery_uses_path_tiebreakers_for_case_insensitive_equal_names() {
    let roots = tempfile::tempdir().expect("workspace roots should be created");
    let alpha_root = roots.path().join("a-root");
    let zebra_root = roots.path().join("m-root");
    let uppercase_alpha_root = roots.path().join("z-root");
    let lowercase_alpha = alpha_root.join("alpha");
    let zebra = zebra_root.join("Zebra");
    let uppercase_alpha = uppercase_alpha_root.join("Alpha");
    fs::create_dir_all(&lowercase_alpha).expect("workspace should be created");
    fs::create_dir_all(&zebra).expect("workspace should be created");
    fs::create_dir_all(&uppercase_alpha).expect("workspace should be created");

    let mut model = TaskQueueModel::new();
    model.discover(vec![
        WorkspaceRoot::new(WorkspaceSource::Github, uppercase_alpha_root),
        WorkspaceRoot::new(WorkspaceSource::Github, zebra_root),
        WorkspaceRoot::new(WorkspaceSource::Github, alpha_root),
    ]);

    let workspaces = model.workspaces_for_source(WorkspaceSource::Github);
    assert_eq!(
        workspaces
            .iter()
            .map(|workspace| workspace.display_name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "Alpha", "Zebra"]
    );
    assert_eq!(
        workspaces[0].path,
        lowercase_alpha
            .canonicalize()
            .expect("workspace path should canonicalize")
    );
    assert_eq!(
        workspaces[1].path,
        uppercase_alpha
            .canonicalize()
            .expect("workspace path should canonicalize")
    );
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
fn task_queue_keeps_tasks_in_memory_without_window_selection() {
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
}

#[test]
fn agent_kinds_map_to_the_expected_wrappers() {
    assert_eq!(AgentKind::Codex.wrapper_name(), "codexauto");
    assert_eq!(AgentKind::ClaudeCode.wrapper_name(), "claudeauto");
}

#[test]
fn codex_launch_command_uses_the_task_markdown_path_in_the_spanish_prompt() {
    let command = build_launch_command(
        AgentKind::Codex,
        Path::new("/tmp/task queue/LOCAL-004/task.md"),
    )
    .expect("absolute paths should build a launch command");

    assert_eq!(
        command,
        "codexauto 'Lee y ejecuta la tarea en /tmp/task queue/LOCAL-004/task.md, incluidos sus adjuntos.'"
    );
}

#[test]
fn claude_code_launch_command_uses_the_task_markdown_path_in_the_spanish_prompt() {
    let command = build_launch_command(
        AgentKind::ClaudeCode,
        Path::new("/tmp/task queue/LOCAL-004/task.md"),
    )
    .expect("absolute paths should build a launch command");

    assert_eq!(
        command,
        "claudeauto 'Lee y ejecuta la tarea en /tmp/task queue/LOCAL-004/task.md, incluidos sus adjuntos.'"
    );
}

#[test]
fn shell_quote_uses_posix_single_quote_escaping() {
    assert_eq!(
        shell_quote("/tmp/O'Reilly/task.md"),
        "'/tmp/O'\\''Reilly/task.md'"
    );
}

#[test]
fn launch_command_keeps_newline_and_shell_metacharacters_in_one_quoted_prompt_argument() {
    let task_path = Path::new("/tmp/task\n; touch /tmp/should-not-run; /safe/task.md");
    let command = build_launch_command(AgentKind::Codex, task_path)
        .expect("absolute paths should build a launch command");

    assert_eq!(
        command,
        "codexauto 'Lee y ejecuta la tarea en /tmp/task\n; touch /tmp/should-not-run; /safe/task.md, incluidos sus adjuntos.'"
    );
}

#[test]
fn launch_command_rejects_relative_task_markdown_paths() {
    let error = build_launch_command(AgentKind::Codex, Path::new("task.md"))
        .expect_err("relative task paths must not produce a launch command");

    assert!(error.to_string().contains("absolute"));
}

#[test]
fn launch_command_rejects_absolute_paths_that_are_not_task_markdown() {
    for path in [Path::new("/tmp/readme.md"), Path::new("/etc/passwd")] {
        let error = build_launch_command(AgentKind::Codex, path)
            .expect_err("only task.md paths should produce a launch command");

        assert_eq!(
            error,
            TaskQueueError::InvalidTaskMarkdownPath(path.to_path_buf())
        );
    }
}

#[test]
fn launch_command_escapes_quote_breakout_attempts_in_task_markdown_paths() {
    let task_path = Path::new("/tmp/task'; touch /tmp/should-not-run; '/task.md");
    let prompt = format!(
        "Lee y ejecuta la tarea en {}, incluidos sus adjuntos.",
        task_path.display()
    );

    let command = build_launch_command(AgentKind::Codex, task_path)
        .expect("an absolute task.md path should build a launch command");

    assert_eq!(command, format!("codexauto {}", shell_quote(&prompt)));
    assert!(command.ends_with('\''));
}
