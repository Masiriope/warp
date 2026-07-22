mod model;

pub(crate) use model::{
    AgentKind, DiscoveredWorkspace, NewTaskInput, Task, TaskAttachment, TaskId, TaskPriority,
    TaskQueueError, TaskQueueModel, TaskStatus, WorkspaceId, WorkspaceRoot, WorkspaceSource,
    default_sources,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
