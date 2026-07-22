# Warp Workspace Task Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a local Markdown-backed task queue to the Masiriope Warp fork, scoped by workspace and able to launch the existing `codexauto` and `claudeauto` workflows in new normal Warp sessions.

**Architecture:** Keep all task-domain and filesystem code in a new `workspace::task_queue` module. Make the existing vertical-tabs panel render Sessions or Tasks, and use a typed modal for task creation. A task launch creates a normal terminal tab at the workspace directory, executes a safely quoted wrapper command, and maps the new terminal pane to the task until its pending command completes.

**Tech Stack:** Rust 2024, WarpUI/GPUI view models and typed actions, existing `Modal` and `FilePickerConfiguration`, `serde`/`serde_json`, `uuid`, standard filesystem APIs, existing Warp terminal command execution APIs.

---

## Locked file structure

| Path | Responsibility |
| --- | --- |
| `app/src/workspace/task_queue/mod.rs` | Public task-queue types, model handle, actions/events and module wiring. |
| `app/src/workspace/task_queue/model.rs` | Workspace discovery, task state transitions, launch tracking and view-facing queries. |
| `app/src/workspace/task_queue/storage.rs` | Atomic Markdown/index/attachment persistence; no Warp UI dependencies. |
| `app/src/workspace/task_queue/command.rs` | Agent command construction and POSIX shell escaping. |
| `app/src/workspace/task_queue/task_dialog.rs` | Native Warp task-creation modal and image/text paste state. |
| `app/src/workspace/task_queue/panel.rs` | Task-mode sidebar and selected-task detail rendering. |
| `app/src/workspace/task_queue/tests.rs` | Unit tests for models, scanning, Markdown, persistence and command construction. |
| `app/src/workspace/task_queue/task_dialog_tests.rs` | Modal state/validation and attachment preview tests. |
| `app/src/workspace/task_queue/panel_tests.rs` | Task-panel rendering decisions and linked-session grouping tests. |
| `app/src/workspace/mod.rs` | Declares/initializes the task-queue singleton. |
| `app/src/workspace/action.rs` | Adds typed workspace actions for Tasks mode and task launches. |
| `app/src/workspace/view.rs` | Owns the queue model, modal lifecycle, terminal launch handoff and terminal completion subscriptions. |
| `app/src/workspace/view/vertical_tabs.rs` | Adds the Sessions/Tareas switch and delegates task-mode rendering. |
| `app/src/workspace/view/vertical_tabs_tests.rs` | Extends mode-selection coverage without changing current Sessions assertions. |
| `app/src/terminal/view.rs` | Exposes a narrowly scoped public helper that sets and executes a trusted task-launch command. |
| `app/src/terminal/view_tests.rs` | Covers the public helper dispatching exactly one shell command. |

The implementation must reuse `Workspace::add_terminal_tab`, `Workspace::insert_subshell_command_and_bootstrap_if_supported`, `TerminalView::set_and_execute_subshell_command`, the existing `Modal` styling, and `FilePickerConfiguration::folders_only()`. Do not create a second terminal view, a generic OS window, or a separate tab sidebar.

### Task 1: Create the pure task-domain model and workspace discovery

**Files:**
- Create: `app/src/workspace/task_queue/mod.rs`
- Create: `app/src/workspace/task_queue/model.rs`
- Create: `app/src/workspace/task_queue/tests.rs`
- Modify: `app/src/workspace/mod.rs`

- [ ] **Step 1: Add the module declaration and test module before implementing behavior.**

```rust
// app/src/workspace/task_queue/mod.rs
mod command;
mod model;
mod panel;
mod storage;
mod task_dialog;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod task_dialog_tests;
#[cfg(test)]
mod panel_tests;

pub(crate) use model::{
    AgentKind, DiscoveredWorkspace, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority,
    TaskQueueModel, TaskStatus, WorkspaceId, WorkspaceSource,
};
```

Add `mod task_queue;` in `app/src/workspace/mod.rs` and initialize one `TaskQueueModel` singleton from `workspace::init`.

- [ ] **Step 2: Write failing discovery/state tests.**

```rust
#[test]
fn discover_workspaces_uses_direct_children_and_groups_by_source() {
    let entries = vec![
        PathBuf::from("/home/sebas/Documents/GitHub/MGA-Portal"),
        PathBuf::from("/home/sebas/Desktop/01_Proyectos_Activos/warp"),
    ];
    let discovered = discover_from_entries(&default_sources(Path::new("/home/sebas")), entries);

    assert_eq!(discovered[0].source, WorkspaceSource::Github);
    assert_eq!(discovered[1].source, WorkspaceSource::ActiveProjects);
    assert_ne!(discovered[0].id, discovered[1].id);
}

#[test]
fn task_status_only_completes_after_explicit_review_confirmation() {
    let mut task = task_with_status(TaskStatus::Pending);
    task.mark_launched("terminal-pane-1").unwrap();
    task.mark_command_finished(true).unwrap();
    assert_eq!(task.status, TaskStatus::ReviewRequired);
    task.mark_done().unwrap();
    assert_eq!(task.status, TaskStatus::Done);
}
```

- [ ] **Step 3: Run the focused tests and verify they fail because the module/types do not exist.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::tests
```

Expected: compilation failure mentioning missing `task_queue` types or functions.

- [ ] **Step 4: Implement the stable types and deterministic discovery.**

```rust
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum TaskStatus {
    Pending,
    InProgress,
    ReviewRequired,
    AttentionRequired,
    Done,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum WorkspaceSource {
    Github,
    ActiveProjects,
}

pub(crate) fn default_sources(home: &Path) -> [WorkspaceRoot; 2] {
    [
        WorkspaceRoot::new(WorkspaceSource::Github, home.join("Documents/GitHub")),
        WorkspaceRoot::new(
            WorkspaceSource::ActiveProjects,
            home.join("Desktop/01_Proyectos_Activos"),
        ),
    ]
}
```

Use the canonical path when available; otherwise retain the original path and expose it as unavailable. Hash the normalized path for `WorkspaceId`, so identical display names never collide. Scan only immediate child directories; sort each group by case-insensitive display name. Do not include a task workspace for the virtual Home conversation context.

- [ ] **Step 5: Run the focused tests and format.**

Run:

```bash
cargo fmt --check
cargo test -p warp --features test-util workspace::task_queue::tests
```

Expected: all new discovery/state tests pass.

- [ ] **Step 6: Commit the domain model.**

```bash
git add app/src/workspace/mod.rs app/src/workspace/task_queue
git commit -m "feat: add workspace task queue domain model"
```

### Task 2: Persist tasks as atomic Markdown plus local attachments

**Files:**
- Create: `app/src/workspace/task_queue/storage.rs`
- Modify: `app/src/workspace/task_queue/model.rs`
- Modify: `app/src/workspace/task_queue/tests.rs`

- [ ] **Step 1: Write failing filesystem tests using a temporary root.**

```rust
#[test]
fn create_task_writes_markdown_and_relative_attachment_links() {
    let temp = tempfile::tempdir().unwrap();
    let store = TaskStore::open(temp.path()).unwrap();
    let task = store.create(new_task_with_png(b"png-bytes")).unwrap();

    let markdown = std::fs::read_to_string(store.task_markdown_path(task.id)).unwrap();
    assert!(markdown.contains("status: pending"));
    assert!(markdown.contains("attachments/captura-001.png"));
    assert!(store.task_attachment_path(task.id, "captura-001.png").exists());
}

#[test]
fn failed_attachment_write_leaves_no_visible_task_directory() {
    let temp = tempfile::tempdir().unwrap();
    let store = TaskStore::open(temp.path()).unwrap();
    assert!(store.create(new_task_with_invalid_attachment()).is_err());
    assert!(store.list_tasks().unwrap().is_empty());
}
```

- [ ] **Step 2: Run the storage tests and verify they fail.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::tests::create_task
```

Expected: failure because `TaskStore` is not implemented.

- [ ] **Step 3: Implement `TaskStore` with a staging directory and Markdown front matter.**

```rust
pub(crate) fn create(&self, input: NewTaskInput) -> Result<Task, TaskQueueError> {
    let task = Task::from_input(input, Utc::now());
    let staging = self.tasks_root.join(format!(".{}.staging", task.id));
    let destination = self.tasks_root.join(task.workspace_id.as_str()).join(task.id.as_str());

    std::fs::create_dir_all(staging.join("attachments"))?;
    write_attachments(&staging.join("attachments"), &task.attachments)?;
    std::fs::write(staging.join("task.md"), render_markdown(&task))?;
    std::fs::rename(staging, destination)?;
    self.write_index_atomically()?;
    Ok(task)
}
```

Implement `write_index_atomically` by writing `workspaces.json.tmp`, flushing it, then renaming it over `workspaces.json`. On every error, remove only the staging path. Store under the active channel's application-data root in `TaskQueue/v1`; never write under the selected project or `.git`.

- [ ] **Step 4: Add round-trip, malformed-Markdown isolation and unavailable-workspace tests.**

Use one valid task and one invalid `task.md`; `list_tasks` must return the valid task and an inspectable load error for the invalid one. Add a test that a missing workspace directory yields `TaskStatus::AttentionRequired` and a human-readable `workspace_path_missing` reason rather than deleting the task.

- [ ] **Step 5: Run the full storage test group.**

Run:

```bash
cargo fmt --check
cargo test -p warp --features test-util workspace::task_queue::tests
```

Expected: persistence, rollback, round-trip and invalid-path cases pass.

- [ ] **Step 6: Commit atomic storage.**

```bash
git add app/src/workspace/task_queue
git commit -m "feat: persist workspace tasks as markdown"
```

### Task 3: Build safe Codex/Claude Code launch commands

**Files:**
- Create: `app/src/workspace/task_queue/command.rs`
- Modify: `app/src/workspace/task_queue/tests.rs`

- [ ] **Step 1: Write failing command-construction tests.**

```rust
#[test]
fn codex_command_passes_an_absolute_task_markdown_prompt() {
    let command = build_launch_command(
        AgentKind::Codex,
        Path::new("/tmp/task queue/LOCAL-004/task.md"),
    );
    assert_eq!(
        command,
        "codexauto 'Lee y ejecuta la tarea en /tmp/task queue/LOCAL-004/task.md, incluidos sus adjuntos.'"
    );
}

#[test]
fn shell_quote_preserves_single_quotes_in_task_paths() {
    assert_eq!(shell_quote("/tmp/O'Reilly/task.md"), "'/tmp/O'\\''Reilly/task.md'");
}
```

- [ ] **Step 2: Run the command tests and verify they fail.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::tests::codex_command
```

Expected: unresolved `build_launch_command` and `shell_quote` symbols.

- [ ] **Step 3: Implement command construction without user-configurable command templates.**

```rust
pub(crate) fn build_launch_command(agent: AgentKind, task_markdown: &Path) -> String {
    let prompt = format!(
        "Lee y ejecuta la tarea en {}, incluidos sus adjuntos.",
        task_markdown.display()
    );
    format!("{} {}", agent.wrapper_name(), shell_quote(&prompt))
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
```

`AgentKind::Codex` maps to `codexauto`; `AgentKind::ClaudeCode` maps to `claudeauto`. Do not use a fictitious `--task` flag. The generated command starts the same interactive CLIs Sebastián starts manually, with the Markdown path as their initial prompt.

- [ ] **Step 4: Run the focused test group.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::tests::shell_quote
cargo test -p warp --features test-util workspace::task_queue::tests::codex_command
```

Expected: both command and escaping tests pass.

- [ ] **Step 5: Commit command construction.**

```bash
git add app/src/workspace/task_queue
git commit -m "feat: compose task launch commands safely"
```

### Task 4: Add the task creation modal with workspace and pasted captures

**Files:**
- Create: `app/src/workspace/task_queue/task_dialog.rs`
- Create: `app/src/workspace/task_queue/task_dialog_tests.rs`
- Modify: `app/src/workspace/task_queue/mod.rs`
- Modify: `app/src/workspace/view.rs`

- [ ] **Step 1: Write modal-state tests before rendering.**

```rust
#[test]
fn create_is_disabled_without_title_or_workspace() {
    let state = TaskDialogState::default();
    assert!(!state.can_create());
}

#[test]
fn pasted_images_keep_insertion_order_and_can_be_removed() {
    let mut state = dialog_state_with_workspace();
    state.add_image(test_png("one.png"));
    state.add_image(test_png("two.png"));
    state.remove_attachment(0);
    assert_eq!(state.attachments()[0].file_name(), "two.png");
}
```

- [ ] **Step 2: Run the modal tests and verify they fail.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::task_dialog_tests
```

Expected: failure because `TaskDialogState` does not exist.

- [ ] **Step 3: Implement a typed modal using the existing Warp modal styling.**

Create `TaskDialog` as a typed action view with these events:

```rust
pub(crate) enum TaskDialogEvent {
    Cancelled,
    Submitted(NewTaskInput),
}
```

Use `Modal::new` following `Workspace::build_session_config_modal`, retaining Warp's backdrop and native type scale. Render a required workspace selector first, then title, priority, context editor, attachment thumbnails and primary Create button. Use `ClipboardContent`/the existing image-paste conversion path to turn each pasted image into a `TaskAttachment`; retain text paste in the context editor. Do not reuse terminal AI attachment state and do not change chat behavior.

- [ ] **Step 4: Wire modal opening and event handling in `Workspace`.**

Add `Workspace::open_task_dialog`, `Workspace::build_task_dialog`, `Workspace::handle_task_dialog_event` and `Workspace::close_task_dialog`. On `Submitted`, call the queue model's atomic create operation, select the new task, close the modal and notify the view. On error, keep the modal open and show an existing `ToastStack` error.

- [ ] **Step 5: Run modal tests and the existing vertical-tabs tests.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::task_dialog_tests
cargo test -p warp --features test-util workspace::view::vertical_tabs_tests
```

Expected: modal state tests and existing Sessions tests pass unchanged.

- [ ] **Step 6: Commit task creation UI.**

```bash
git add app/src/workspace/task_queue app/src/workspace/view.rs
git commit -m "feat: add workspace task creation dialog"
```

### Task 5: Render Tasks mode in the existing vertical-tabs panel

**Files:**
- Create: `app/src/workspace/task_queue/panel.rs`
- Create: `app/src/workspace/task_queue/panel_tests.rs`
- Modify: `app/src/workspace/view/vertical_tabs.rs`
- Modify: `app/src/workspace/view/vertical_tabs_tests.rs`
- Modify: `app/src/workspace/action.rs`
- Modify: `app/src/workspace/view.rs`

- [ ] **Step 1: Write pure rendering-decision tests.**

```rust
#[test]
fn task_mode_lists_only_selected_workspace_tasks() {
    let model = queue_with_tasks_for_two_workspaces();
    let rows = task_rows_for_workspace(&model, workspace_id("MGA-Portal"));
    assert_eq!(rows.iter().map(|row| &row.title).collect::<Vec<_>>(), ["Revisar firmas"]);
}

#[test]
fn sessions_mode_keeps_task_linked_rows_out_of_regular_rows() {
    let rows = partition_session_rows(session_rows_with_one_linked_task());
    assert_eq!(rows.task_sessions.len(), 1);
    assert_eq!(rows.regular_sessions.len(), 2);
}
```

- [ ] **Step 2: Run the new panel tests and verify they fail.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::panel_tests
```

Expected: unresolved task-panel helpers.

- [ ] **Step 3: Add an explicit panel mode and delegate rendering.**

Introduce a small `VerticalSidebarMode { Sessions, Tasks }` field in the vertical-tabs panel state. In `render_vertical_tabs_panel`, replace only the body below the control bar:

```rust
let panel_body = match state.sidebar_mode {
    VerticalSidebarMode::Sessions => render_groups(state, workspace, app),
    VerticalSidebarMode::Tasks => task_queue::panel::render_task_panel(state, workspace, app),
};
```

Keep the panel's existing `ClippedScrollable`, theme colors, resize behavior and Search/controls. The Tasks renderer owns the workspace dropdown, New task button, priority/status rows and selected-task detail. A row dispatches selection only; the detail dispatches launch actions.

- [ ] **Step 4: Add task actions and preserve existing session behavior.**

Add only these typed `WorkspaceAction` variants: `ShowTaskQueue`, `ShowSessions`, `OpenTaskDialog`, `SelectTask(TaskId)`, `LaunchTask { task_id, agent }`, `OpenLinkedTaskSession(TaskId)`, `MarkTaskDone(TaskId)`. Handle them beside the existing vertical-tab actions. No action may classify manually created terminal sessions.

- [ ] **Step 5: Run panel and regression tests.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::panel_tests
cargo test -p warp --features test-util workspace::view::vertical_tabs_tests
cargo test -p warp --features test-util workspace::view_tests::test_vertical_tabs_panel_defaults_open_for_new_window_when_vertical_tabs_enabled
```

Expected: Tasks-mode tests pass and all original vertical-tabs behavior remains green.

- [ ] **Step 6: Commit the Tasks panel.**

```bash
git add app/src/workspace/action.rs app/src/workspace/view.rs app/src/workspace/view/vertical_tabs.rs app/src/workspace/view/vertical_tabs_tests.rs app/src/workspace/task_queue
git commit -m "feat: show workspace tasks in vertical sidebar"
```

### Task 6: Launch a linked normal terminal session and update task state

**Files:**
- Modify: `app/src/terminal/view.rs`
- Modify: `app/src/terminal/view_tests.rs`
- Modify: `app/src/workspace/view.rs`
- Modify: `app/src/workspace/task_queue/model.rs`
- Modify: `app/src/workspace/task_queue/tests.rs`

- [ ] **Step 1: Write failing terminal-command and transition tests.**

```rust
#[test]
fn task_launch_command_is_submitted_once() {
    let (terminal, executed) = bootstrapped_terminal_with_command_subscription();
    terminal.update(&mut app, |view, ctx| {
        view.execute_task_launch_command("codexauto 'Read /tmp/task.md'", ShellType::Zsh, ctx);
    });
    assert_eq!(executed.borrow().as_slice(), ["codexauto 'Read /tmp/task.md'"]);
}

#[test]
fn successful_linked_command_moves_task_to_review() {
    let mut task = launched_task();
    task.finish_linked_command(true).unwrap();
    assert_eq!(task.status, TaskStatus::ReviewRequired);
}
```

- [ ] **Step 2: Run the focused tests and verify they fail.**

Run:

```bash
cargo test -p warp --features test-util terminal::view_tests::task_launch_command
cargo test -p warp --features test-util workspace::task_queue::tests::successful_linked_command
```

Expected: missing public task-launch helper and missing transition method.

- [ ] **Step 3: Expose the narrow terminal helper using existing execution internals.**

Add this public wrapper in `TerminalView`; it must delegate to the existing private method and not write raw PTY bytes:

```rust
pub fn execute_task_launch_command(
    &mut self,
    command: &str,
    shell_type: ShellType,
    ctx: &mut ViewContext<Self>,
) {
    self.set_and_execute_subshell_command(command, shell_type, ctx);
}
```

This reuses Warp's pending-command handling, bootstrap support and `PendingCommandCompleted` event instead of simulating Return keystrokes.

- [ ] **Step 4: Implement the Workspace launch handoff.**

`Workspace::launch_task` must:

1. reject unavailable workspaces and already-linked active tasks with a toast and no terminal mutation;
2. call the existing normal terminal-tab path with the selected workspace directory as its initial directory (add a task-specific `NewSessionSource`/working-directory path instead of mutating global terminal preferences);
3. save the resulting `TerminalPaneId`/session identity to the task before executing the command;
4. call `execute_task_launch_command` on that new terminal using the command from `command.rs`;
5. select the new normal session and switch the sidebar to Sessions.

Subscribe to that terminal's `TerminalView::Event::PendingCommandCompleted`. If the tracked command succeeds, transition InProgress → ReviewRequired; if it fails, the workspace disappears, or the PTY reports `PtySpawnFailed`, transition to AttentionRequired with the user-visible reason. Remove the pane-to-task mapping after either terminal event. Do not treat manual terminal commands as task events.

- [ ] **Step 5: Run terminal and queue regressions.**

Run:

```bash
cargo test -p warp --features test-util terminal::view_tests::task_launch_command
cargo test -p warp --features test-util workspace::task_queue::tests
cargo test -p warp --features test-util workspace::view_tests
```

Expected: task command dispatches once, linked-state transitions are correct, and ordinary terminal tests remain green.

- [ ] **Step 6: Commit terminal integration.**

```bash
git add app/src/terminal/view.rs app/src/terminal/view_tests.rs app/src/workspace/view.rs app/src/workspace/task_queue
git commit -m "feat: launch linked task sessions"
```

### Task 7: Add linked-session grouping, review completion and full regression coverage

**Files:**
- Modify: `app/src/workspace/view/vertical_tabs.rs`
- Modify: `app/src/workspace/view/vertical_tabs_tests.rs`
- Modify: `app/src/workspace/task_queue/panel.rs`
- Modify: `app/src/workspace/task_queue/panel_tests.rs`
- Modify: `app/src/workspace/task_queue/model.rs`
- Modify: `app/src/workspace/task_queue/tests.rs`

- [ ] **Step 1: Write the final behavior tests.**

```rust
#[test]
fn task_sessions_group_is_absent_when_no_linked_task_is_running() {
    assert!(task_session_group(empty_linked_sessions()).is_none());
}

#[test]
fn mark_done_is_manual_and_removes_task_from_pending_filter() {
    let mut queue = queue_with_review_task();
    queue.mark_done(task_id("LOCAL-004")).unwrap();
    assert!(queue.pending_for(workspace_id("MGA-Portal")).is_empty());
    assert_eq!(queue.done_for(workspace_id("MGA-Portal")).len(), 1);
}
```

- [ ] **Step 2: Run final behavior tests and verify they fail.**

Run:

```bash
cargo test -p warp --features test-util workspace::task_queue::panel_tests
```

Expected: missing grouping/filter behavior.

- [ ] **Step 3: Render the temporary `Tareas en curso` session group.**

In Sessions mode, partition normal rows from linked active task rows. Render a regular Warp group header named `Tareas en curso` above ordinary sessions only when the linked collection is non-empty. Rows use the task title, task status and local identifier; deliberately do not display Codex or Anthropic product icons. A task row opens its normal session. A task detail with an active link shows `Abrir sesión`, not another launch button.

- [ ] **Step 4: Add manual completion and attention-required detail actions.**

The detail view renders `Marcar como hecha` only for ReviewRequired/AttentionRequired tasks. Marking done persists Markdown first, refreshes filters and does not close the associated terminal. Retrying an AttentionRequired task requires an explicit launch action and keeps prior failure text in the Markdown history section.

- [ ] **Step 5: Run all targeted tests and static checks.**

Run:

```bash
cargo fmt --check
cargo clippy -p warp --features test-util --lib -- -D warnings
cargo test -p warp --features test-util workspace::task_queue
cargo test -p warp --features test-util workspace::view::vertical_tabs_tests
cargo test -p warp --features test-util terminal::view_tests::task_launch_command
```

Expected: formatting, clippy and all task/sidebar/terminal tests pass.

- [ ] **Step 6: Commit task completion and linked-session presentation.**

```bash
git add app/src/workspace/view/vertical_tabs.rs app/src/workspace/view/vertical_tabs_tests.rs app/src/workspace/task_queue
git commit -m "feat: track task session completion"
```

### Task 8: Verify the isolated local build and manual MVP workflow

**Files:**
- Modify: `docs/superpowers/specs/2026-07-22-warp-workspace-task-queue-design.md` only if verification reveals a design correction.
- Create: `docs/superpowers/verification/2026-07-22-warp-workspace-task-queue.md`

- [ ] **Step 1: Confirm the development target is isolated before launching it.**

Read `app/src/bin/local.rs` and verify its embedded identity remains `WarpLocal` / `dev.warp.Warp-Local`. Do not run, close, restart, replace or inspect private content of the currently-open stable Warp application.

- [ ] **Step 2: Build and run only the local target with a fresh app-data profile.**

Run:

```bash
WARP_SKIP_COMMON_SKILLS_INSTALL=1 ./script/run
```

The repository's macOS runner builds a real `.app`; when `warp-channel-config` is available it selects the `warp` local binary, whose plist identifies it as `WarpLocal` / `dev.warp.Warp-Local`. Verify that identity in the launched app before interacting with it. Never overwrite `/Applications/Warp.app`.

- [ ] **Step 3: Execute the manual acceptance script in the isolated build.**

1. Open Tareas and confirm GitHub and 01_Proyectos_Activos workspaces are listed by source.
2. Confirm a General conversation starts in `~` and has no task queue.
3. Create an MGA-Portal task with High priority, Markdown text and three pasted screenshots.
4. Confirm the Markdown and attachments are stored only in the isolated app-data root, not in MGA-Portal.
5. Launch with Codex; verify a new normal session opens in MGA-Portal and the command begins with `codexauto`.
6. Return to Sessions and verify the `Tareas en curso` separator appears without agent logos.
7. Exit the agent command; verify the task becomes Para revisar, then mark it Hecha manually.
8. Start `codexauto` manually in another normal session and verify it is not added to the task group.
9. Repeat the launch flow with Claude Code.

- [ ] **Step 4: Record evidence and commit verification notes.**

Include build command, test commands, pass/fail result, screenshots from the isolated profile and any known limitation. Do not place screenshots or task data in the Warp source repository unless they are deliberately redacted test fixtures.

```bash
git add docs/superpowers/verification
git commit -m "docs: verify workspace task queue mvp"
git push
```

## Plan self-review

### Spec coverage

- Per-workspace queue, fixed source roots and Home-only general conversations: Tasks 1 and 5.
- Native create dialog, priority, context and multiple pasted captures: Task 4.
- Local Markdown, attachments, atomic writes and no repository changes: Task 2.
- Exact existing agent wrappers, task Markdown prompt and safe quoting: Task 3.
- New normal terminal session, no manual-command detection, bidirectional task/session links: Task 6.
- Pending → In progress → Review required → Done, failures and duplicate-launch protection: Tasks 1, 6 and 7.
- Existing Warp visual style and session separator with no product icons: Task 5 and Task 7.
- Separate development build and no disruption to running Warp: Task 8.
- Jira exclusion: stated in scope; no implementation task intentionally adds it.

### Placeholder and consistency check

The plan defines each new module, its tests, the public names used across tasks, exact commands, state names and the final manual acceptance script. No task refers to Jira implementation, a nonexistent `--task` CLI flag or automatic classification of manually started agents.
