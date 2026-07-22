mod command;
mod model;
mod panel;
mod storage;
mod task_dialog;

pub(crate) use command::build_launch_command;
#[cfg(test)]
pub(crate) use command::shell_quote;
pub(crate) use model::{
    AgentKind, DiscoveredWorkspace, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority,
    TaskQueueError, TaskQueueModel, TaskQueuePersistError, TaskStatus, TaskWorkspace, WorkspaceId,
    WorkspaceRoot, WorkspaceSource, default_sources,
};
pub(crate) use panel::{
    TaskPanelSelection, TaskSessionMetadata, TaskSessionRow, render_task_queue_panel,
    task_session_group,
};
pub(crate) use storage::{TaskLoadError, TaskStore, TaskStoreError};
#[cfg(test)]
pub(crate) use task_dialog::TaskDialogState;
pub(crate) use task_dialog::{TaskDialog, TaskDialogEvent};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "task_dialog_tests.rs"]
mod task_dialog_tests;

#[cfg(test)]
#[path = "panel_tests.rs"]
mod panel_tests;
