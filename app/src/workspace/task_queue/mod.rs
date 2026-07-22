mod command;
mod model;
mod panel;
mod storage;
mod task_dialog;

pub(crate) use command::{build_launch_command, shell_quote};
pub(crate) use model::{
    AgentKind, DiscoveredWorkspace, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority,
    TaskQueueError, TaskQueueModel, TaskStatus, TaskWorkspace, WorkspaceId, WorkspaceRoot,
    WorkspaceSource, default_sources,
};
pub(crate) use panel::render_task_queue_panel;
pub(crate) use storage::{TaskLoadError, TaskStore, TaskStoreError};
pub(crate) use task_dialog::{TaskDialog, TaskDialogEvent, TaskDialogState};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "task_dialog_tests.rs"]
mod task_dialog_tests;

#[cfg(test)]
#[path = "panel_tests.rs"]
mod panel_tests;
