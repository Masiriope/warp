mod command;
mod model;
mod storage;

pub(crate) use command::{build_launch_command, shell_quote};
pub(crate) use model::{
    AgentKind, DiscoveredWorkspace, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority,
    TaskQueueError, TaskQueueModel, TaskStatus, TaskWorkspace, WorkspaceId, WorkspaceRoot,
    WorkspaceSource, default_sources,
};
pub(crate) use storage::{TaskLoadError, TaskStore, TaskStoreError};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
